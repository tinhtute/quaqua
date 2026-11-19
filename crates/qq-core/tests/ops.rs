//! Worker operations and farm views: brief check and watchdog decisions,
//! cleanup/stop, four-per-tab placement, status from the database. Agents run
//! on FakeHerdr; pane operations run on a test-local in-memory host because the
//! frozen herdr seam has no pane verbs yet.

use std::path::Path;
use std::process::Command;

use qq_core::config;
use qq_core::db;
use qq_core::driver::Registry;
use qq_core::grid::{self, HerdrPanes, PaneOps};
use qq_core::herdr::{FakeHerdr, HerdrClient};
use qq_core::status;
use qq_core::task::cleanup::{self, OpsError};
use qq_core::task::prompt::{
    self, Allowance, Clock, Exit, Observed, ProviderError, Shape, Tuning, WaitCfg,
};
use qq_core::task::spawn::{spawn, Role, SpawnSpec};
use qq_core::task::worker_flow::place_pane;
use rusqlite::Connection;

const BRIEF: &str = "Topology: solo — one unit\nPlaybook: feature\nContext: none, test\n\
Goal: g\nHard constraints: h\nCurrent design (open to challenge): d\n\
Unknowns: u\nEvidence: e\nTime budget: 15 min\nCost budget: $2\n";

fn seeded(tag: &str) -> (std::path::PathBuf, Connection) {
    let dir = std::env::temp_dir().join(format!("qq-ops-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let conn = db::open(&dir.join(db::path::DB_FILE)).unwrap();
    let team = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/team.toml"),
    )
    .unwrap();
    let mut c = conn;
    config::seed(&mut c, &team).unwrap();
    (dir, c)
}

fn add_agent(conn: &Connection, name: &str, role: &str, kind: &str, project: Option<i64>) {
    conn.execute(
        "INSERT INTO agent (name, role, kind, project_id, started_ms) VALUES (?1, ?2, ?3, ?4, 1)",
        rusqlite::params![name, role, kind, project],
    )
    .unwrap();
}

fn add_project(conn: &Connection, slug: &str) -> i64 {
    conn.execute(
        "INSERT INTO project (slug, created_ms) VALUES (?1, 1)",
        [slug],
    )
    .unwrap();
    conn.last_insert_rowid()
}

fn ended(conn: &Connection, name: &str) -> bool {
    conn.query_row(
        "SELECT ended_ms IS NOT NULL FROM agent WHERE name = ?1",
        [name],
        |r| r.get(0),
    )
    .unwrap()
}

/// Pane helpers over FakeHerdr.
fn occupy(h: &FakeHerdr, pane: &str, agent: &str) {
    let r = h.agent_start(agent, "pi", pane, 1, &[]).unwrap();
    assert!(r.ok, "{}", r.body);
}

fn tabs(h: &FakeHerdr) -> Vec<String> {
    let mut t: Vec<String> = HerdrPanes(h)
        .panes()
        .unwrap()
        .into_iter()
        .map(|p| p.tab)
        .collect();
    t.sort();
    t.dedup();
    t
}

#[test]
fn prompt_check_brief_and_resume() {
    let ok = prompt::check_brief(BRIEF, None, None).unwrap();
    assert_eq!((ok.budget_sec, ok.source), (900, "brief"));
    assert!(ok.warnings.is_empty());
    // the env override wins, no budget line falls to the default
    assert_eq!(
        prompt::check_brief(BRIEF, Some(60), None).unwrap().source,
        "env"
    );
    let no_time = BRIEF.replace("Time budget: 15 min\n", "");
    let d = prompt::check_brief(&no_time, None, None).unwrap();
    assert_eq!((d.budget_sec, d.source), (2700, "default"));
    // rejected briefs
    for bad in [
        BRIEF.replace("Topology: solo — one unit\n", ""),
        BRIEF.replace("Goal: g\n", ""),
        format!("{BRIEF}Must fix: urgent\n"),
        format!("{BRIEF}Review rounds: 0\n"),
        BRIEF.replace("15 min", "soon"),
    ] {
        assert!(prompt::check_brief(&bad, None, None).is_err(), "{bad}");
    }
    // advisory gaps only warn
    let w = prompt::check_brief(
        &BRIEF.replace("Playbook: feature", "Playbook: none"),
        None,
        None,
    )
    .unwrap();
    assert_eq!(w.warnings.len(), 1);
    // a '<…>' placeholder in Context warns
    let c = prompt::check_brief(
        &BRIEF.replace("Context: none, test", "Context: <files / PR pointers…>"),
        None,
        None,
    )
    .unwrap();
    assert!(
        c.warnings.iter().any(|w| w.contains("Context")),
        "{:?}",
        c.warnings
    );
    // with a playbooks index, a named playbook must resolve
    let pb = std::env::temp_dir().join(format!("qq-ops-playbooks-{}", std::process::id()));
    std::fs::create_dir_all(&pb).unwrap();
    std::fs::write(pb.join("feature.md"), "---\nname: feature\n---\n").unwrap();
    assert!(prompt::check_brief(BRIEF, None, Some(&pb)).is_ok());
    let missing = BRIEF.replace("Playbook: feature", "Playbook: nope");
    let e = prompt::check_brief(&missing, None, Some(&pb)).unwrap_err();
    assert!(e.contains("nope"), "{e}");
    // `none, <reason>` is the documented trivial-unit form: accepted, no warning,
    // even with an index; bare `none` still warns; an unknown name still fails
    let nr = BRIEF.replace("Playbook: feature", "Playbook: none, one-command unit");
    let ok = prompt::check_brief(&nr, None, Some(&pb)).unwrap();
    assert!(ok.warnings.is_empty(), "{:?}", ok.warnings);
    let bare = BRIEF.replace("Playbook: feature", "Playbook: none");
    let w = prompt::check_brief(&bare, None, Some(&pb)).unwrap();
    assert!(
        w.warnings.iter().any(|m| m.contains("needs a reason")),
        "{:?}",
        w.warnings
    );
    assert!(prompt::check_brief(&missing, None, Some(&pb)).is_err());
    // an empty reason warns as a missing reason; a '<…>' reason as a placeholder
    for v in ["none,", "none,   ", "none;"] {
        let b = BRIEF.replace("Playbook: feature", &format!("Playbook: {v}"));
        for idx in [Some(pb.as_path()), None] {
            let w = prompt::check_brief(&b, None, idx).unwrap();
            assert!(
                w.warnings.iter().any(|m| m.contains("needs a reason")),
                "{v:?}: {:?}",
                w.warnings
            );
        }
    }
    for v in ["none, <reason>", "none, <…>"] {
        let b = BRIEF.replace("Playbook: feature", &format!("Playbook: {v}"));
        for idx in [Some(pb.as_path()), None] {
            let w = prompt::check_brief(&b, None, idx).unwrap();
            assert!(
                w.warnings.iter().any(|m| m.contains("placeholder")),
                "{v:?}: {:?}",
                w.warnings
            );
        }
    }
    // `none <word>` is not the none form: it goes to the name lookup
    let nw = BRIEF.replace("Playbook: feature", "Playbook: none of the above");
    assert!(prompt::check_brief(&nw, None, Some(&pb)).is_err());
    // no index at all: the name is not checked
    assert!(prompt::check_brief(&missing, None, Some(&pb.join("absent"))).is_ok());

    // watchdog codes
    let base = Observed {
        budget_sec: 900,
        ..Default::default()
    };
    assert_eq!(prompt::decide(&base), None);
    let code = |o: Observed| prompt::decide(&o).map(Exit::code);
    assert_eq!(
        code(Observed {
            report: Some("Outcome: COMPLETE\n## Lessons\nnone\n".into()),
            ..base.clone()
        }),
        Some(0)
    );
    assert_eq!(
        code(Observed {
            report: Some("done, all green\n".into()),
            ..base.clone()
        }),
        Some(6)
    );
    assert_eq!(
        code(Observed {
            elapsed_sec: 7200,
            ..base.clone()
        }),
        Some(2)
    );
    assert_eq!(
        code(Observed {
            since_change_sec: 600,
            ..base.clone()
        }),
        Some(3)
    );
    assert_eq!(
        code(Observed {
            idle_sec: 120,
            ..base.clone()
        }),
        Some(3)
    );
    assert_eq!(
        code(Observed {
            elapsed_sec: 900,
            ..base.clone()
        }),
        Some(4)
    );
    assert_eq!(
        code(Observed {
            blocked: true,
            ..base.clone()
        }),
        Some(5)
    );
    assert_eq!(
        code(Observed {
            blocked: true,
            provider_error: true,
            ..base.clone()
        }),
        Some(7)
    );
    assert_eq!(
        code(Observed {
            core_died: true,
            ..base.clone()
        }),
        Some(8)
    );
    // a worker that is not blocked and not stalled is never a provider error
    assert_eq!(
        code(Observed {
            provider_error: true,
            ..base.clone()
        }),
        None
    );

    // resume: the same observation under an extended allowance keeps waiting
    let over = Observed {
        elapsed_sec: 950,
        budget_sec: 900,
        ..Default::default()
    };
    assert_eq!(code(over.clone()), Some(4));
    let a = Allowance(900).extend(300);
    assert_eq!(
        prompt::decide(&Observed {
            budget_sec: a.0,
            ..over
        }),
        None
    );
    assert!(prompt::parse_extend("0").is_err());
    assert_eq!(prompt::parse_extend("90"), Ok(90));
}

fn git(dir: &Path, args: &[&str]) -> String {
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
    String::from_utf8_lossy(&o.stdout).trim().to_string()
}

/// Panes herdr shows that are not this workspace's workers, in real shapes.
struct Foreign {
    cos: String,
    human: String,
    other_farm: String,
}

fn foreign(h: &FakeHerdr) -> Foreign {
    Foreign {
        cos: h.seed_pane(
            "main",
            Some("claude"),
            &[
                ("grid_role", "cos"),
                ("grid_owner", "me"),
                ("grid_farm", "f1"),
            ],
        ),
        human: h.seed_pane("main", None, &[]),
        other_farm: h.seed_pane(
            "other-workers",
            Some("claude"),
            &[
                ("grid_role", "worker"),
                ("grid_owner", "me"),
                ("grid_farm", "f2"),
            ],
        ),
    }
}

fn survive(h: &FakeHerdr, f: &Foreign) {
    for (what, p) in [
        ("cos", &f.cos),
        ("human", &f.human),
        ("other farm", &f.other_farm),
    ] {
        assert!(h.has_pane(p), "the {what} pane must survive");
    }
}

fn set_pane(conn: &Connection, name: &str, pane: &str) {
    conn.execute(
        "UPDATE agent SET pane_id = ?2 WHERE name = ?1",
        [name, pane],
    )
    .unwrap();
}

fn scope_f1() -> grid::Scope {
    grid::Scope {
        owner: None,
        farm: Some("f1".into()),
    }
}

/// A repo with a committed worker worktree `wt-w1` on branch `quaqua/w1`,
/// plus the agent record, a recorded pane and a sidecar.
fn worker_fixture(
    tag: &str,
    herdr: &FakeHerdr,
) -> (
    std::path::PathBuf,
    Connection,
    std::path::PathBuf,
    std::path::PathBuf,
    String,
) {
    let (dir, conn) = seeded(tag);
    let repo = dir.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    std::fs::write(repo.join("a"), "a").unwrap();
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-q", "-m", "base"]);
    let wt = dir.join("wt-w1");
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
    std::fs::write(wt.join("b"), "b").unwrap();
    git(&wt, &["add", "."]);
    git(&wt, &["commit", "-q", "-m", "work"]);
    add_agent(&conn, "w1", "peer", "tieude", None);
    let pane = herdr.seed_pane(
        "repo-workers",
        Some("pi"),
        &[("grid_role", "worker"), ("grid_farm", "f1")],
    );
    set_pane(&conn, "w1", &pane);
    std::fs::write(dir.join(".w1.budget"), "60\n").unwrap();
    std::fs::write(dir.join(".w1.provider_retry"), "2\n").unwrap();
    (dir, conn, repo, wt, pane)
}

fn target<'a>(
    dir: &'a Path,
    repo: &'a Path,
    wt: &'a Path,
    allow_dirty: bool,
) -> cleanup::Target<'a> {
    cleanup::Target {
        repo,
        worktree: wt,
        runs_dir: dir,
        name: "w1",
        allow_dirty,
    }
}

#[test]
fn cleanup_keeps_branch() {
    let herdr = FakeHerdr::new();
    let f = foreign(&herdr);
    let (dir, conn, repo, wt, pane) = worker_fixture("cleanup", &herdr);
    let panes = HerdrPanes(&herdr);
    let t = target(&dir, &repo, &wt, false);

    cleanup::cleanup_worker(&conn, &panes, &scope_f1(), &t, 5).unwrap();
    assert!(!wt.exists());
    assert!(git(&repo, &["branch", "--list", "quaqua/w1"]).contains("quaqua/w1"));
    assert!(ended(&conn, "w1"));
    assert!(!herdr.has_pane(&pane), "cleanup closes the recorded pane");
    assert!(!dir.join(".w1.budget").exists() && !dir.join(".w1.provider_retry").exists());
    survive(&herdr, &f);
    // a repeat converges
    cleanup::cleanup_worker(&conn, &panes, &scope_f1(), &t, 6).unwrap();
    survive(&herdr, &f);
}

#[test]
fn cleanup_refuses_dirty_and_wrong_repo_and_foreign_pane() {
    let herdr = FakeHerdr::new();
    let f = foreign(&herdr);
    let (dir, conn, repo, wt, pane) = worker_fixture("cleanup-refuse", &herdr);
    let panes = HerdrPanes(&herdr);

    // dirty (an untracked file counts): refused, listing the path, nothing touched
    std::fs::write(wt.join("scratch.txt"), "x").unwrap();
    let r = cleanup::cleanup_worker(
        &conn,
        &panes,
        &scope_f1(),
        &target(&dir, &repo, &wt, false),
        5,
    );
    assert_eq!(r, Err(OpsError::Dirty(vec!["scratch.txt".into()])));
    assert!(wt.join("scratch.txt").exists() && herdr.has_pane(&pane));
    assert!(!ended(&conn, "w1") && dir.join(".w1.budget").exists());

    // a repo that owns neither the worktree nor the branch: refused untouched
    let other = dir.join("other");
    std::fs::create_dir_all(&other).unwrap();
    git(&other, &["init", "-q", "-b", "main"]);
    std::fs::write(other.join("a"), "a").unwrap();
    git(&other, &["add", "."]);
    git(&other, &["commit", "-q", "-m", "base"]);
    let gone = dir.join("no-such-wt");
    let r = cleanup::cleanup_worker(
        &conn,
        &panes,
        &scope_f1(),
        &target(&dir, &other, &gone, true),
        5,
    );
    assert!(matches!(r, Err(OpsError::WrongRepo(_))), "{r:?}");
    assert!(herdr.has_pane(&pane) && !ended(&conn, "w1"));

    // the explicit flag discards the uncommitted path; git never runs --force
    cleanup::cleanup_worker(
        &conn,
        &panes,
        &scope_f1(),
        &target(&dir, &repo, &wt, true),
        6,
    )
    .unwrap();
    assert!(!wt.exists() && ended(&conn, "w1") && !herdr.has_pane(&pane));
    survive(&herdr, &f);

    // a record that names a CoS pane never gets that pane closed
    add_agent(&conn, "w2", "peer", "tieude", None);
    set_pane(&conn, "w2", &f.cos);
    let wt2 = dir.join("wt-w2");
    git(
        &repo,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "quaqua/w2",
            &wt2.to_string_lossy(),
        ],
    );
    let t2 = cleanup::Target {
        name: "w2",
        ..target(&dir, &repo, &wt2, false)
    };
    cleanup::cleanup_worker(&conn, &panes, &scope_f1(), &t2, 7).unwrap();
    assert!(ended(&conn, "w2"));
    survive(&herdr, &f);
}

fn project_with_repo(conn: &Connection, slug: &str, repo: &Path) -> i64 {
    let p = add_project(conn, slug);
    conn.execute(
        "UPDATE project SET repo_path = ?2 WHERE id = ?1",
        rusqlite::params![p, repo.to_string_lossy()],
    )
    .unwrap();
    p
}

/// A PaneOps whose close is a no-op: herdr said ok but the pane stayed.
struct LyingClose<'a>(HerdrPanes<'a>);
impl PaneOps for LyingClose<'_> {
    fn panes(&self) -> std::io::Result<Vec<grid::Pane>> {
        self.0.panes()
    }
    fn create_tab(&self, l: &str) -> std::io::Result<grid::Pane> {
        self.0.create_tab(l)
    }
    fn split(&self, a: &str, d: grid::Dir) -> std::io::Result<grid::Pane> {
        self.0.split(a, d)
    }
    fn close_pane(&self, _: &str) -> std::io::Result<()> {
        Ok(())
    }
    fn claim(&self, p: &str, s: &grid::Scope) -> std::io::Result<()> {
        self.0.claim(p, s)
    }
}

#[test]
fn cleanup_closes_the_live_pane_when_the_recorded_id_is_stale() {
    let herdr = FakeHerdr::new();
    let f = foreign(&herdr);
    let (dir, conn, repo, wt, pane) = worker_fixture("cleanup-stale", &herdr);
    herdr.agent_start("w1", "pi", &pane, 1000, &[]).unwrap();
    set_pane(&conn, "w1", "p999"); // the pane moved; the record kept the old id
    let panes = HerdrPanes(&herdr);
    let t = target(&dir, &repo, &wt, false);
    cleanup::cleanup_worker_live(&conn, &panes, &herdr, &scope_f1(), &t, 5).unwrap();
    assert!(
        !herdr.has_pane(&pane),
        "the live pane is closed, not only the recorded id"
    );
    assert!(ended(&conn, "w1") && !wt.exists());
    survive(&herdr, &f);
}

#[test]
fn cleanup_closes_an_orphan_pane_whose_worktree_and_branch_are_gone() {
    let herdr = FakeHerdr::new();
    let f = foreign(&herdr);
    let (dir, conn, repo, wt, pane) = worker_fixture("cleanup-orphan", &herdr);
    let p = project_with_repo(&conn, "proj", &repo);
    conn.execute("UPDATE agent SET project_id = ?1 WHERE name = 'w1'", [p])
        .unwrap();
    herdr.agent_start("w1", "pi", &pane, 1000, &[]).unwrap();
    git(
        &repo,
        &["worktree", "remove", "--force", &wt.to_string_lossy()],
    );
    git(&repo, &["branch", "-D", "quaqua/w1"]);
    let panes = HerdrPanes(&herdr);
    let t = target(&dir, &repo, &wt, false);
    cleanup::cleanup_worker_live(&conn, &panes, &herdr, &scope_f1(), &t, 5).unwrap();
    assert!(!herdr.has_pane(&pane) && ended(&conn, "w1"));
    survive(&herdr, &f);
}

#[test]
fn cleanup_names_what_remains_instead_of_reporting_cleaned() {
    let herdr = FakeHerdr::new();
    let (dir, conn, repo, wt, pane) = worker_fixture("cleanup-remains", &herdr);
    let panes = LyingClose(HerdrPanes(&herdr));
    let t = target(&dir, &repo, &wt, false);
    let r = cleanup::cleanup_worker(&conn, &panes, &scope_f1(), &t, 5);
    assert_eq!(r, Err(OpsError::Remains(vec![format!("pane {pane}")])));
    assert!(
        !ended(&conn, "w1"),
        "the agent row stays so a retry converges"
    );
}

#[test]
fn lead_cleanup_removes_the_lead_worktree_or_names_it() {
    let herdr = FakeHerdr::new();
    let (dir, conn, repo, _wt, _pane) = worker_fixture("leadwt", &herdr);
    let p = project_with_repo(&conn, "proj", &repo);
    add_agent(&conn, "proj-lead", "lead", "ni", Some(p));
    let lwt = dir.join(".wt").join("proj-lead");
    git(
        &repo,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "quaqua/proj-lead",
            &lwt.to_string_lossy(),
        ],
    );
    let panes = HerdrPanes(&herdr);
    cleanup::cleanup_lead(&conn, &panes, &scope_f1(), "proj", 9).unwrap();
    assert!(
        !lwt.exists(),
        "a Lead cleanup that reports cleaned leaves no worktree"
    );
    assert!(ended(&conn, "proj-lead"));
}

fn lead_fixture(
    tag: &str,
    herdr: &FakeHerdr,
) -> (std::path::PathBuf, Connection, std::path::PathBuf, String) {
    let (dir, conn, repo, _wt, _pane) = worker_fixture(tag, herdr);
    let p = project_with_repo(&conn, "proj", &repo);
    add_agent(&conn, "proj-lead", "lead", "ni", Some(p));
    let mine = herdr.seed_pane(
        "repo-workers",
        Some("claude"),
        &[("grid_role", "lead"), ("grid_farm", "f1")],
    );
    set_pane(&conn, "proj-lead", &mine);
    (dir, conn, repo, mine)
}

#[test]
fn lead_cleanup_closes_its_own_lead_pane() {
    let herdr = FakeHerdr::new();
    let f = foreign(&herdr);
    let (_d, conn, _repo, mine) = lead_fixture("lead-own-pane", &herdr);
    let panes = HerdrPanes(&herdr);
    cleanup::cleanup_lead(&conn, &panes, &scope_f1(), "proj", 9).unwrap();
    assert!(!herdr.has_pane(&mine), "the Lead's own pane is closed");
    assert!(ended(&conn, "proj-lead"));
    survive(&herdr, &f);
}

#[test]
fn lead_cleanup_closes_a_moved_lead_pane_by_live_lookup() {
    let herdr = FakeHerdr::new();
    let (_d, conn, _repo, mine) = lead_fixture("lead-live", &herdr);
    herdr
        .agent_start("proj-lead", "ni", &mine, 1000, &[])
        .unwrap();
    set_pane(&conn, "proj-lead", "p999");
    let panes = HerdrPanes(&herdr);
    cleanup::cleanup_lead_live(&conn, &panes, &herdr, &scope_f1(), "proj", 9).unwrap();
    assert!(!herdr.has_pane(&mine) && ended(&conn, "proj-lead"));
}

#[test]
fn cleanup_refuses_to_claim_cleaned_when_herdr_lookup_fails_and_the_record_is_stale() {
    let herdr = FakeHerdr::new();
    let (dir, conn, repo, wt, pane) = worker_fixture("cleanup-lookup", &herdr);
    herdr.agent_start("w1", "pi", &pane, 1000, &[]).unwrap();
    set_pane(&conn, "w1", "p999");
    herdr.set_fault(qq_core::herdr::Verb::Get, std::io::ErrorKind::TimedOut);
    let panes = HerdrPanes(&herdr);
    let t = target(&dir, &repo, &wt, false);
    let r = cleanup::cleanup_worker_live(&conn, &panes, &herdr, &scope_f1(), &t, 5);
    assert!(
        matches!(&r, Err(OpsError::Remains(v)) if v.iter().any(|m| m.contains("pane"))),
        "{r:?}"
    );
    assert!(!ended(&conn, "w1") && herdr.has_pane(&pane));
}

#[test]
fn cleanup_falls_back_to_the_recorded_pane_when_herdr_lost_the_agent() {
    let herdr = FakeHerdr::new();
    let (dir, conn, repo, wt, pane) = worker_fixture("cleanup-lost", &herdr);
    // herdr has no agent `w1` (agent_not_found); the record is right
    let panes = HerdrPanes(&herdr);
    let t = target(&dir, &repo, &wt, false);
    cleanup::cleanup_worker_live(&conn, &panes, &herdr, &scope_f1(), &t, 5).unwrap();
    assert!(!herdr.has_pane(&pane) && ended(&conn, "w1"));
}

#[test]
fn lead_cleanup_rerun_removes_the_worktree_an_older_cleanup_left() {
    let herdr = FakeHerdr::new();
    let (dir, conn, repo, _mine) = lead_fixture("lead-rerun", &herdr);
    let lwt = dir.join(".wt").join("proj-lead");
    git(
        &repo,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "quaqua/proj-lead",
            &lwt.to_string_lossy(),
        ],
    );
    conn.execute("UPDATE agent SET ended_ms = 3 WHERE name = 'proj-lead'", [])
        .unwrap(); // an older cleanup ended the row and left the worktree
    let panes = HerdrPanes(&herdr);
    cleanup::cleanup_lead(&conn, &panes, &scope_f1(), "proj", 9).unwrap();
    assert!(!lwt.exists(), "the rerun finishes the job");
    // a repeat converges
    cleanup::cleanup_lead(&conn, &panes, &scope_f1(), "proj", 10).unwrap();
}

#[test]
fn lead_cleanup_of_an_unknown_project_is_an_error() {
    let herdr = FakeHerdr::new();
    let (_d, conn) = seeded("lead-unknown");
    let r = cleanup::cleanup_lead(&conn, &HerdrPanes(&herdr), &scope_f1(), "nope", 9);
    assert!(r.is_err(), "{r:?}");
}

#[test]
fn lead_cleanup_finds_a_worktree_registered_outside_the_convention_path() {
    let herdr = FakeHerdr::new();
    let (dir, conn, repo, _mine) = lead_fixture("lead-elsewhere", &herdr);
    let lwt = dir.join("side-wt");
    git(
        &repo,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "quaqua/proj-lead",
            &lwt.to_string_lossy(),
        ],
    );
    let panes = HerdrPanes(&herdr);
    cleanup::cleanup_lead(&conn, &panes, &scope_f1(), "proj", 9).unwrap();
    assert!(!lwt.exists());
    assert!(!git(&repo, &["worktree", "list", "--porcelain"]).contains("side-wt"));
}

#[test]
fn lead_cleanup_names_a_dirty_worktree_and_releases_nothing() {
    let herdr = FakeHerdr::new();
    let (dir, conn, repo, mine) = lead_fixture("lead-dirty", &herdr);
    let lwt = dir.join(".wt").join("proj-lead");
    git(
        &repo,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "quaqua/proj-lead",
            &lwt.to_string_lossy(),
        ],
    );
    std::fs::write(lwt.join("scratch.txt"), "x").unwrap();
    let panes = HerdrPanes(&herdr);
    let r = cleanup::cleanup_lead(&conn, &panes, &scope_f1(), "proj", 9);
    assert_eq!(r, Err(OpsError::Dirty(vec!["scratch.txt".into()])));
    assert!(lwt.exists() && herdr.has_pane(&mine) && !ended(&conn, "proj-lead"));
}

#[test]
fn cleanup_with_the_repo_gone_names_the_worktree_that_remains() {
    let herdr = FakeHerdr::new();
    let (dir, conn, repo, wt, pane) = worker_fixture("cleanup-norepo", &herdr);
    let gone = dir.join("repo-moved");
    std::fs::rename(&repo, &gone).unwrap();
    let panes = HerdrPanes(&herdr);
    let t = target(&dir, &repo, &wt, true);
    let r = cleanup::cleanup_worker(&conn, &panes, &scope_f1(), &t, 5);
    assert!(
        matches!(&r, Err(OpsError::Remains(v)) if v.iter().any(|m| m.starts_with("worktree "))),
        "{r:?}"
    );
    assert!(!herdr.has_pane(&pane) && !ended(&conn, "w1"));
}

fn scope_me() -> grid::Scope {
    grid::Scope {
        owner: Some("me".into()),
        farm: Some("f1".into()),
    }
}

#[test]
fn cleanup_names_a_live_pane_the_scope_rejects() {
    let herdr = FakeHerdr::new();
    let (dir, conn, repo, wt, _pane) = worker_fixture("cleanup-oos", &herdr);
    let theirs = herdr.seed_pane(
        "repo-workers",
        Some("pi"),
        &[
            ("grid_role", "worker"),
            ("grid_owner", "x"),
            ("grid_farm", "f1"),
        ],
    );
    herdr.agent_start("w1", "pi", &theirs, 1000, &[]).unwrap();
    set_pane(&conn, "w1", "p999");
    let panes = HerdrPanes(&herdr);
    let t = target(&dir, &repo, &wt, false);
    let r = cleanup::cleanup_worker_live(&conn, &panes, &herdr, &scope_me(), &t, 5);
    assert!(
        matches!(&r, Err(OpsError::Remains(v)) if v.iter().any(|m| m.contains(&theirs) && m.contains("out of scope"))),
        "{r:?}"
    );
    assert!(herdr.has_pane(&theirs) && !ended(&conn, "w1"));
}

#[test]
fn lead_cleanup_names_a_live_lead_pane_the_scope_rejects() {
    let herdr = FakeHerdr::new();
    let (_d, conn, _repo, mine) = lead_fixture("lead-oos", &herdr);
    herdr
        .agent_start("proj-lead", "ni", &mine, 1000, &[])
        .unwrap();
    let panes = HerdrPanes(&herdr);
    let r = cleanup::cleanup_lead_live(
        &conn,
        &panes,
        &herdr,
        &grid::Scope {
            owner: None,
            farm: Some("f2".into()),
        },
        "proj",
        9,
    );
    assert!(
        matches!(&r, Err(OpsError::Remains(v)) if v.iter().any(|m| m.contains(&mine) && m.contains("out of scope"))),
        "{r:?}"
    );
    assert!(herdr.has_pane(&mine) && !ended(&conn, "proj-lead"));
}

#[test]
fn lead_rerun_ignores_the_stale_recorded_pane_of_an_ended_row() {
    let herdr = FakeHerdr::new();
    let (_d, conn, _repo, mine) = lead_fixture("lead-ended-stale", &herdr);
    // the ended Lead's old id is now a live worker pane in scope
    let reused = herdr.seed_pane(
        "repo-workers",
        Some("pi"),
        &[("grid_role", "worker"), ("grid_farm", "f1")],
    );
    set_pane(&conn, "proj-lead", &reused);
    conn.execute("UPDATE agent SET ended_ms = 3 WHERE name = 'proj-lead'", [])
        .unwrap();
    let panes = HerdrPanes(&herdr);
    cleanup::cleanup_lead_live(&conn, &panes, &herdr, &scope_f1(), "proj", 9).unwrap();
    assert!(
        herdr.has_pane(&reused),
        "an ended row's recorded id is not trusted"
    );
    let _ = mine;
}

#[test]
fn stop_never_sends_ctrl_c_and_leaves_the_worker_alive() {
    let (_dir, conn) = seeded("stop");
    let herdr = FakeHerdr::new();
    let f = foreign(&herdr);
    // claude has no verified stop sequence: refused before anything is sent,
    // working or idle (pi's `esc` is the driver's; see below)
    for (name, kind, harness) in [("c1", "ni", "claude")] {
        add_agent(&conn, name, "peer", kind, None);
        let pane = herdr.seed_pane("r-workers", None, &[]);
        set_pane(&conn, name, &pane);
        herdr.agent_start(name, harness, &pane, 1, &[]).unwrap();
        for status in ["working", "idle"] {
            herdr.set_status(name, status);
            let r = cleanup::stop_worker(&conn, &herdr, name);
            assert_eq!(
                r,
                Err(OpsError::Unverified(harness.into())),
                "{name} {status}"
            );
        }
    }
    assert!(
        herdr.keys().is_empty(),
        "nothing is sent for an unverified harness"
    );

    // pi stops on esc, the one source being its driver
    add_agent(&conn, "p1", "peer", "tieude", None);
    let pane = herdr.seed_pane("r-workers", None, &[]);
    set_pane(&conn, "p1", &pane);
    herdr.agent_start("p1", "pi", &pane, 1, &[]).unwrap();
    herdr.set_status("p1", "working");
    herdr.idle_on_keys("p1", &["esc"]);
    cleanup::stop_worker(&conn, &herdr, "p1").unwrap();
    assert_eq!(
        herdr.keys(),
        vec![("p1".to_string(), vec!["esc".to_string()])]
    );

    // opencode stops on esc esc; the worker, its pane and its record stay
    conn.execute(
        "INSERT INTO kind(name, harness, model) VALUES ('oc', 'opencode', 'm')",
        [],
    )
    .unwrap();
    add_agent(&conn, "o1", "peer", "oc", None);
    let pane = herdr.seed_pane("r-workers", None, &[]);
    set_pane(&conn, "o1", &pane);
    herdr.agent_start("o1", "opencode", &pane, 1, &[]).unwrap();
    herdr.set_status("o1", "working");
    herdr.idle_on_keys("o1", &["esc", "esc"]);
    cleanup::stop_worker(&conn, &herdr, "o1").unwrap();
    assert_eq!(
        herdr.keys().last(),
        Some(&("o1".to_string(), vec!["esc".to_string(), "esc".to_string()]))
    );
    assert!(herdr
        .keys()
        .iter()
        .all(|(_, k)| k.iter().all(|x| x != "ctrl+c")));
    assert!(herdr.has_pane(&pane) && !ended(&conn, "o1"));
    assert!(
        herdr.agent_get("o1").unwrap().ok,
        "the agent is still alive"
    );
    survive(&herdr, &f);
}

#[test]
fn grid_four_per_tab() {
    let herdr = FakeHerdr::new();
    let f = foreign(&herdr);
    let panes = HerdrPanes(&herdr);
    let mut got = Vec::new();
    // five spawns with no pane named: each lands where grid placement says
    let (sd, sconn) = seeded("spawnplace");
    sconn
        .execute(
            "INSERT INTO project (slug, repo_path, created_ms) VALUES ('repo', '/x/repo', 1)",
            [],
        )
        .unwrap();
    let drivers = Registry::native();
    for i in 0..5 {
        let name = format!("w{i}");
        let id = place_pane(&sconn, &herdr, &name, Some(1)).unwrap();
        spawn(
            &sconn,
            &herdr,
            &drivers,
            &SpawnSpec {
                name: &name,
                kind: "tieude",
                role: Role::Peer,
                pane: &id,
                project_id: Some(1),
                spawner_id: None,
            },
            2,
        )
        .unwrap();
        got.push(id);
    }
    // a repeat of a spawn keeps its pane rather than placing a new one
    assert_eq!(place_pane(&sconn, &herdr, "w2", Some(1)).unwrap(), got[2]);
    assert_eq!(
        tabs(&herdr),
        vec!["main", "other-workers", "repo-workers", "repo-workers-2"]
    );
    let per_tab = |t: &str| panes.panes().unwrap().iter().filter(|p| p.tab == t).count();
    assert_eq!((per_tab("repo-workers"), per_tab("repo-workers-2")), (4, 1));
    // the spawn recorded its pane, and placement tagged it as a worker pane
    let recorded: String = sconn
        .query_row("SELECT pane_id FROM agent WHERE name = 'w3'", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(recorded, got[3]);
    let tok = herdr.tokens(&got[3]);
    assert_eq!(tok.get("grid_role").map(String::as_str), Some("worker"));
    assert_eq!(tok.get("grid_farm"), grid::farm_id(&sd).as_ref());
    // a closed pane's slot is refilled before a third tab opens
    panes.close_pane(&got[1]).unwrap();
    let scope = grid::Scope::from_runs_dir(&sd);
    let id = grid::acquire(&panes, "repo", &scope, &sd).unwrap();
    let tab = panes
        .panes()
        .unwrap()
        .into_iter()
        .find(|p| p.id == id)
        .unwrap()
        .tab;
    assert_eq!(tab, "repo-workers");
    assert_eq!(grid::place("repo", &[4, 4]).tab, "repo-workers-3");

    // reconcile closes the pane of an agent the DB ended, and only that one
    sconn
        .execute("UPDATE agent SET ended_ms = 2 WHERE name = 'w0'", [])
        .unwrap();
    let ops = grid::reconcile(&panes, &sconn, &scope).unwrap();
    assert_eq!(ops, vec![grid::Op::Close(got[0].clone())]);
    assert!(grid::reconcile(&panes, &sconn, &scope).unwrap().is_empty());
    survive(&herdr, &f);
}

#[test]
fn reconcile_closes_only_recorded_panes_of_this_scope() {
    let herdr = FakeHerdr::new();
    let f = foreign(&herdr);
    let (_d, conn) = seeded("reconcile");
    let panes = HerdrPanes(&herdr);
    let mine = herdr.seed_pane(
        "repo-workers",
        Some("pi"),
        &[("grid_role", "worker"), ("grid_farm", "f1")],
    );
    let untagged = herdr.seed_pane("repo-workers", None, &[]);
    let live = herdr.seed_pane(
        "repo-workers",
        Some("pi"),
        &[("grid_role", "worker"), ("grid_farm", "f1")],
    );
    let unrecorded = herdr.seed_pane(
        "repo-workers",
        Some("pi"),
        &[("grid_role", "worker"), ("grid_farm", "f1")],
    );
    // ended agents recorded for our panes and (worst case) the foreign agent panes
    for (n, p) in [
        ("e-mine", &mine),
        ("e-untagged", &untagged),
        ("e-cos", &f.cos),
        ("e-other", &f.other_farm),
    ] {
        add_agent(&conn, n, "peer", "tieude", None);
        set_pane(&conn, n, p);
        conn.execute("UPDATE agent SET ended_ms = 2 WHERE name = ?1", [n])
            .unwrap();
    }
    add_agent(&conn, "alive", "peer", "tieude", None);
    set_pane(&conn, "alive", &live);

    let ops = grid::reconcile(&panes, &conn, &scope_f1()).unwrap();
    let mut closed: Vec<String> = ops.into_iter().map(|grid::Op::Close(p)| p).collect();
    closed.sort();
    let mut want = vec![mine.clone(), untagged.clone()];
    want.sort();
    assert_eq!(closed, want);
    // The human's pane is in no spawn record, so it survives with the CoS pane,
    // the other farm's worker, the live agent's pane and the unrecorded pane.
    assert!(herdr.has_pane(&f.human) && herdr.has_pane(&f.cos) && herdr.has_pane(&f.other_farm));
    assert!(herdr.has_pane(&live) && herdr.has_pane(&unrecorded));
    assert!(grid::reconcile(&panes, &conn, &scope_f1())
        .unwrap()
        .is_empty());
}

#[test]
fn concurrent_acquires_never_share_a_pane() {
    let herdr = FakeHerdr::new();
    let dir = std::env::temp_dir().join(format!("qq-ops-lock-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let scope = grid::Scope::default();
    let ids: Vec<String> = std::thread::scope(|s| {
        let hs: Vec<_> = (0..8)
            .map(|_| s.spawn(|| grid::acquire(&HerdrPanes(&herdr), "repo", &scope, &dir).unwrap()))
            .collect();
        hs.into_iter().map(|h| h.join().unwrap()).collect()
    });
    let mut uniq = ids.clone();
    uniq.sort();
    uniq.dedup();
    assert_eq!(uniq.len(), 8, "{ids:?}");
    let panes = HerdrPanes(&herdr).panes().unwrap();
    let n = |t: &str| panes.iter().filter(|p| p.tab == t).count();
    assert_eq!((n("repo-workers"), n("repo-workers-2")), (4, 4));
}

#[test]
fn placement_splits_a_two_by_two() {
    let herdr = FakeHerdr::new();
    let dir = std::env::temp_dir().join(format!("qq-ops-2x2-{}", std::process::id()));
    let scope = grid::Scope::default();
    let ids: Vec<String> = (0..4)
        .map(|_| grid::acquire(&HerdrPanes(&herdr), "repo", &scope, &dir).unwrap())
        .collect();
    assert_eq!(
        herdr.splits(),
        vec![
            (ids[0].clone(), "right".to_string()),
            (ids[1].clone(), "down".to_string()),
            (ids[0].clone(), "down".to_string()),
        ]
    );
}

#[test]
fn status_reads_db() {
    let (_d, conn) = seeded("status");
    let pid = add_project(&conn, "zimage");
    add_agent(&conn, "lead1", "lead", "ni", Some(pid));
    add_agent(&conn, "w1", "peer", "tieude", Some(pid));
    conn.execute("UPDATE agent SET ended_ms = 3 WHERE name = 'w1'", [])
        .unwrap();
    conn.execute(
        "INSERT INTO run (agent_id, kind, harness, model, started_ms, ended_ms, outcome, tokens_in, tokens_out, cost_usd, cost_state) \
         SELECT id, 'tieude', 'pi', 'm', 1, 2, 'verified', 10, 20, NULL, 'unknown' FROM agent WHERE name = 'w1'",
        [],
    )
    .unwrap();
    let open = status::render(&status::read(&conn, false).unwrap(), false);
    assert!(open.contains("lead1 lead ni zimage"));
    assert!(
        !open.contains("w1"),
        "ended agents hide without --all: {open}"
    );
    let all = status::render(&status::read(&conn, true).unwrap(), true);
    assert!(all.contains("w1 peer tieude zimage (ended)"), "{all}");
    assert!(
        all.contains("cost=unknown"),
        "an unreported cost is never 0: {all}"
    );
    assert!(all.contains("in=10 out=20"), "{all}");
}

#[test]
fn pane_metadata_merges_and_repeats() {
    let herdr = FakeHerdr::new();
    let pane = HerdrPanes(&herdr).create_tab("t").unwrap().id;
    let tokens = |r: qq_core::herdr::Reply| {
        let v: serde_json::Value = serde_json::from_str(&r.body).unwrap();
        v.pointer("/result/pane/tokens").unwrap().clone()
    };
    herdr
        .pane_report_metadata(&pane, "s", &[("grid_role", "lead")], Some("L"))
        .unwrap();
    herdr
        .pane_report_metadata(&pane, "s", &[("grid_farm", "f")], None)
        .unwrap();
    herdr
        .pane_report_metadata(&pane, "s", &[("grid_farm", "f")], None)
        .unwrap();
    let got = herdr.pane_get(&pane).unwrap();
    assert_eq!(
        tokens(got.clone()),
        serde_json::json!({"grid_role": "lead", "grid_farm": "f"})
    );
    assert!(got.body.contains("\"display_agent\":\"L\""));
    assert!(!herdr.pane_get("nope").unwrap().ok);
}

/// A clock the test owns: sleeping advances it and runs `on_sleep(now)`.
struct FakeClock<'a> {
    now: std::cell::Cell<u64>,
    on_sleep: Box<dyn Fn(u64) + 'a>,
}

impl Clock for FakeClock<'_> {
    fn now_sec(&self) -> u64 {
        self.now.get()
    }
    fn sleep_ms(&self, ms: u64) {
        self.now.set(self.now.get() + ms / 1000);
        (self.on_sleep)(self.now.get());
    }
}

fn wait_cfg<'a>(
    runs: &'a Path,
    journal: &'a dyn Fn(&str, serde_json::Value),
    budget_sec: u64,
) -> WaitCfg<'a> {
    WaitCfg {
        name: "w",
        runs_dir: runs,
        report: runs.join("w.md"),
        worktree: None,
        budget_sec,
        poll_ms: 5_000,
        retry_base_ms: 60_000,
        retry_max: 4,
        harness: None,
        journal,
    }
}

#[test]
fn prompt_wait_exits_on_report_and_on_timebox() {
    let (runs, _c) = seeded("wait");
    let h = FakeHerdr::new();
    occupy(&h, "p1", "w");
    h.set_status("w", "working");
    let journal = |_: &str, _: serde_json::Value| {};

    // A report appears at t=20: exit 0, and no nudge was due (budget 100).
    let r = runs.join("w.md");
    let clock = FakeClock {
        now: 1000.into(),
        on_sleep: Box::new(|now| {
            if now >= 1020 {
                std::fs::write(
                    &r,
                    "Outcome: COMPLETE\n\n## Blockers\nnone\n\n## Lessons\nnone\n",
                )
                .unwrap();
            }
        }),
    };
    let cfg = wait_cfg(&runs, &journal, 100);
    assert_eq!(prompt::wait(&h, &clock, &cfg), Exit::ReportReady);
    assert!(h.prompts().is_empty());
    std::fs::remove_file(&r).unwrap();
    for f in [".w.budget", ".w.elapsed", ".w.nudged"] {
        let _ = std::fs::remove_file(runs.join(f));
    }

    // Silence past the budget: one 80% nudge, then exit 4. (An idle agent: a
    // working one counts as progress and earns the grace.)
    h.set_status("w", "idle");
    let clock = FakeClock {
        now: 5000.into(),
        // the fake flips an agent to working on our nudge; a silent one goes idle again
        on_sleep: Box::new(|_| h.set_status("w", "idle")),
    };
    let cfg = wait_cfg(&runs, &journal, 60);
    assert_eq!(prompt::wait(&h, &clock, &cfg), Exit::BudgetExceeded);
    assert_eq!(clock.now_sec() - 5000, 60);
    let nudges: Vec<_> = h
        .prompts()
        .into_iter()
        .filter(|(_, t)| t.starts_with("Time check"))
        .collect();
    assert_eq!(nudges.len(), 1);
    assert_eq!(
        std::fs::read_to_string(runs.join(".w.budget"))
            .unwrap()
            .trim(),
        "60"
    );
    assert!(runs.join(".w.nudged").exists());
}

#[test]
fn prompt_wait_retries_a_provider_error_then_escalates() {
    let (runs, _c) = seeded("retry");
    let h = FakeHerdr::new();
    occupy(&h, "p1", "w");
    h.set_status("w", "idle");
    let events = std::cell::RefCell::new(Vec::<String>::new());
    let journal = |k: &str, _: serde_json::Value| events.borrow_mut().push(k.to_string());
    let err = runs.join(".w.error");
    std::fs::write(&err, "retryable 529 0\n").unwrap();
    let again = err.clone();
    // Each retry clears the verdict; the harness errors again after a sleep.
    let clock = FakeClock {
        now: 0.into(),
        on_sleep: Box::new(|_| {
            let _ = std::fs::write(&again, "retryable 529 0\n");
            // a harness that errors again goes idle again after our "continue"
            h.set_status("w", "idle");
        }),
    };
    let cfg = wait_cfg(&runs, &journal, 7200);
    assert_eq!(prompt::wait(&h, &clock, &cfg), Exit::ProviderError);
    let ev = events.borrow();
    assert_eq!(ev.iter().filter(|k| *k == "provider_retry").count(), 4);
    assert_eq!(ev.last().map(String::as_str), Some("provider_escalate"));
    assert_eq!(
        h.prompts().iter().filter(|(_, t)| t == "continue").count(),
        4
    );
}

#[test]
fn provider_error_classifier_table() {
    use prompt::classify;
    let r = |h, pane| classify(h, pane).map(|d| (d.error, d.exhausted));
    let (opencode, pi, claude) = (Some(Shape::Opencode), Some(Shape::Pi), Some(Shape::Claude));
    let retry = |s| Some((ProviderError::Retryable(s), false));
    let fatal = |s| Some((ProviderError::Fatal(s), false));
    assert_eq!(
        r(opencode, "Error: 429 rate limit exceeded"),
        retry("rate-limit")
    );
    assert_eq!(
        r(opencode, "Error: 401 invalid api key"),
        fatal("signed-out")
    );
    assert_eq!(r(opencode, "Error: 402 payment required"), fatal("billing"));
    assert_eq!(r(pi, "Error: 503: service unavailable"), retry("5xx"));
    assert_eq!(
        r(claude, "⎿ API Error: 529 overloaded_error"),
        retry("overloaded")
    );
    assert_eq!(
        r(claude, "Not logged in · Please run /login"),
        fatal("signed-out")
    );
    // a spent harness ladder is flagged
    assert_eq!(
        r(
            opencode,
            "Error: Retry budget exhausted after 10 retries: 529 overloaded"
        ),
        Some((ProviderError::Retryable("overloaded"), true))
    );
    // worker output that only mentions provider words never classifies
    assert_eq!(r(opencode, "429 tests run, 0 failed"), None);
    assert_eq!(r(claude, "I fixed the API error handling"), None);
    // fatal beats an earlier retryable one; only the last 15 lines count
    assert_eq!(
        r(
            opencode,
            "Error: 529 overloaded\nError: 401 invalid api key"
        ),
        fatal("signed-out")
    );
    let old = format!("Error: 529 overloaded\n{}", "ok\n".repeat(15));
    assert_eq!(r(opencode, &old), None);
    // structured record: fail closed, stale-free parse
    assert_eq!(
        prompt::parse_error_record("retryable 429 3").map(|d| (d.error, d.exhausted)),
        Some((ProviderError::Retryable("rate-limit"), true))
    );
    assert_eq!(
        prompt::parse_error_record("fatal 529 0").map(|d| d.error),
        Some(ProviderError::Fatal("harness-fatal"))
    );
    assert!(prompt::parse_error_record("retryable x 0").is_none());
}

#[test]
fn prompt_wait_classifies_the_pane_tail() {
    let (runs, _c) = seeded("pane");
    let h = FakeHerdr::new();
    occupy(&h, "p1", "w");
    h.set_status("w", "idle");
    h.set_pane_text("p1", "Error: 529 overloaded\n");
    let events = std::cell::RefCell::new(Vec::<String>::new());
    let journal = |k: &str, _: serde_json::Value| events.borrow_mut().push(k.to_string());
    // The harness echoes our "continue" under the old error; nothing newer follows.
    let echo = || h.set_pane_text("p1", "Error: 529 overloaded\ncontinue\n");
    let clock = FakeClock {
        now: 0.into(),
        on_sleep: Box::new(|_| {
            echo();
            h.set_status("w", "idle");
        }),
    };
    let mut cfg = wait_cfg(&runs, &journal, 7200);
    cfg.harness = Some(Shape::Opencode);
    // idle 120s -> the pane reads retryable -> one provider_retry and one
    // "continue"; the old error sits above the echo, so it never counts twice
    // and the wait ends as a plain stall (exit 3).
    assert_eq!(prompt::wait(&h, &clock, &cfg), Exit::Stalled);
    assert_eq!(*events.borrow(), vec!["provider_retry".to_string()]);
    assert_eq!(
        h.prompts().iter().filter(|(_, t)| t == "continue").count(),
        1
    );
}

#[test]
fn prompt_sends_the_brief_once_and_resume_sends_nothing() {
    let (runs, _c) = seeded("send");
    let h = FakeHerdr::new();
    occupy(&h, "p1", "w");
    h.set_status("w", "idle");
    let tuning = Tuning {
        poll_ms: 5_000,
        retry_base_ms: 60_000,
        retry_max: 4,
    };
    let args = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    let report = runs.join("w.md");
    let ready = || {
        std::fs::write(
            &report,
            "Outcome: COMPLETE\n\n## Blockers\nnone\n\n## Lessons\nnone\n",
        )
    };
    // stale counters of an earlier task are cleared by a fresh prompt
    std::fs::write(runs.join(".w.nudged"), "").unwrap();

    let clock = FakeClock {
        now: 0.into(),
        on_sleep: Box::new(|_| {
            ready().unwrap();
        }),
    };
    let out = prompt::prompt_run(&h, &clock, &runs, &args(&["w", BRIEF]), &tuning).unwrap();
    assert!(out.ends_with("w.md"), "{out}");
    let sent = h.prompts();
    assert_eq!(sent.len(), 1);
    assert!(sent[0].1.starts_with("Topology: solo"), "{}", sent[0].1);
    assert!(sent[0].1.contains("~/quaqua/runs/w.md"));
    assert!(!runs.join(".w.nudged").exists());
    assert_eq!(
        std::fs::read_to_string(runs.join(".w.budget"))
            .unwrap()
            .trim(),
        "900"
    );

    // --resume re-enters the wait and sends nothing; --extend grows the allowance
    let clock = FakeClock {
        now: 0.into(),
        on_sleep: Box::new(|_| {}),
    };
    prompt::prompt_run(
        &h,
        &clock,
        &runs,
        &args(&["w", "--resume", "--extend", "60"]),
        &tuning,
    )
    .unwrap();
    assert_eq!(h.prompts().len(), 1);
    assert_eq!(
        std::fs::read_to_string(runs.join(".w.budget"))
            .unwrap()
            .trim(),
        "960"
    );

    // a resume that carries a brief, or a fresh prompt without one, is a usage error
    for bad in [&["w", "--resume", BRIEF][..], &["w"][..]] {
        let e = prompt::prompt_run(&h, &clock, &runs, &args(bad), &tuning).unwrap_err();
        assert_eq!(e.code, 1);
    }
    // a brief that fails the check sends nothing
    let e =
        prompt::prompt_run(&h, &clock, &runs, &args(&["w", "no headers"]), &tuning).unwrap_err();
    assert_eq!(e.code, 1);
    assert_eq!(h.prompts().len(), 1);
}

#[test]
fn core_died_needs_a_db_open_agent_that_herdr_lost() {
    let (runs, conn) = seeded("coredied");
    let h = FakeHerdr::new();
    let f = foreign(&h);
    add_agent(&conn, "w", "peer", "tieude", None);
    let journal = |_: &str, _: serde_json::Value| {};
    let clock = || FakeClock {
        now: 0.into(),
        on_sleep: Box::new(|_| {}),
    };
    // DB says open, herdr has no such agent: it died without an end record.
    let cfg = wait_cfg(&runs, &journal, 7200);
    assert_eq!(prompt::wait(&h, &clock(), &cfg), Exit::CoreDied);
    // DB says ended: the same absence is only a stall.
    conn.execute("UPDATE agent SET ended_ms = 2 WHERE name = 'w'", [])
        .unwrap();
    assert_eq!(prompt::wait(&h, &clock(), &cfg), Exit::Stalled);
    survive(&h, &f);
}

#[test]
fn resume_continues_the_provider_retry_count() {
    let (runs, _c) = seeded("resumeretry");
    let h = FakeHerdr::new();
    let f = foreign(&h);
    occupy(&h, "p1", "w");
    h.set_status("w", "idle");
    // three retries were spent before the wait was resumed
    std::fs::write(runs.join(".w.provider_retry"), "3\n").unwrap();
    std::fs::write(runs.join(".w.error"), "retryable 529 0\n").unwrap();
    let again = runs.join(".w.error");
    let events = std::cell::RefCell::new(Vec::<String>::new());
    let journal = |k: &str, _: serde_json::Value| events.borrow_mut().push(k.to_string());
    let clock = FakeClock {
        now: 0.into(),
        on_sleep: Box::new(|_| {
            let _ = std::fs::write(&again, "retryable 529 0\n");
            h.set_status("w", "idle");
        }),
    };
    let cfg = wait_cfg(&runs, &journal, 7200);
    assert_eq!(prompt::wait(&h, &clock, &cfg), Exit::ProviderError);
    // 3 spent + 1 = the cap of 4: one more retry, not four
    assert_eq!(
        events
            .borrow()
            .iter()
            .filter(|k| *k == "provider_retry")
            .count(),
        1
    );
    survive(&h, &f);
}

#[test]
fn auto_extend_needs_a_diff_that_moved_since_wait_start() {
    let (runs, _c) = seeded("autoext");
    let repo = runs.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    std::fs::write(repo.join("a"), "a").unwrap();
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-q", "-m", "base"]);
    let h = FakeHerdr::new();
    occupy(&h, "p1", "w");
    h.set_status("w", "idle");
    let journal = |_: &str, _: serde_json::Value| {};
    let reset = || {
        for f in [
            ".w.budget",
            ".w.elapsed",
            ".w.nudged",
            ".w.grace",
            ".w.autoext",
        ] {
            let _ = std::fs::remove_file(runs.join(f));
        }
    };
    let mut cfg = wait_cfg(&runs, &journal, 60);
    cfg.worktree = Some(repo.clone());

    // the worktree is dirty from the start and never moves: no extension
    std::fs::write(repo.join("a"), "dirty").unwrap();
    let clock = FakeClock {
        now: 5000.into(),
        on_sleep: Box::new(|_| h.set_status("w", "idle")),
    };
    assert_eq!(prompt::wait(&h, &clock, &cfg), Exit::BudgetExceeded);
    assert_eq!(clock.now_sec() - 5000, 60);

    // the diff moves mid-wait: one 25% extension, then the deadline holds
    reset();
    let clock = FakeClock {
        now: 5000.into(),
        on_sleep: Box::new(|now| {
            h.set_status("w", "idle");
            if now == 5030 {
                std::fs::write(repo.join("a"), "moved").unwrap();
            }
        }),
    };
    assert_eq!(prompt::wait(&h, &clock, &cfg), Exit::BudgetExceeded);
    assert_eq!(clock.now_sec() - 5000, 75);
}

#[test]
fn working_is_progress_and_the_pane_tail_is_the_second_signal() {
    let (runs, _c) = seeded("progress");
    let h = FakeHerdr::new();
    occupy(&h, "p1", "w");
    let journal = |_: &str, _: serde_json::Value| {};
    // working with a silent pane and no diff: never a stall; only the hard cap
    h.set_status("w", "working");
    let clock = FakeClock {
        now: 0.into(),
        on_sleep: Box::new(|_| {}),
    };
    let cfg = wait_cfg(&runs, &journal, 100_000);
    assert_eq!(prompt::wait(&h, &clock, &cfg), Exit::HardCap);

    // neither working nor idle (status unknown), but the pane keeps printing:
    // progress. The 600 s stall clock starts only once the pane goes quiet.
    for f in [".w.budget", ".w.elapsed", ".w.nudged"] {
        let _ = std::fs::remove_file(runs.join(f));
    }
    h.set_status("w", "unknown");
    let n = std::cell::Cell::new(0u32);
    let clock = FakeClock {
        now: 0.into(),
        on_sleep: Box::new(|now| {
            if now < 1500 {
                n.set(n.get() + 1);
                h.set_pane_text("p1", &format!("line {}\n", n.get()));
            }
        }),
    };
    assert_eq!(prompt::wait(&h, &clock, &cfg), Exit::Stalled);
    assert!(clock.now_sec() >= 2000, "stalled at {}", clock.now_sec());
}

#[test]
fn lead_cleanup_closes_only_the_recorded_pane() {
    let herdr = FakeHerdr::new();
    let f = foreign(&herdr);
    let (_d, conn) = seeded("leadclean");
    let p = add_project(&conn, "proj");
    let mine = herdr.seed_pane(
        "repo-workers",
        Some("claude"),
        &[("grid_role", "lead"), ("grid_farm", "f1")],
    );
    let bystander = herdr.seed_pane("repo-workers", Some("pi"), &[("grid_role", "worker")]);
    add_agent(&conn, "proj-lead", "lead", "ni", Some(p));
    set_pane(&conn, "proj-lead", &mine);
    let panes = HerdrPanes(&herdr);
    cleanup::cleanup_lead(&conn, &panes, &scope_f1(), "proj", 9).unwrap();
    // the Lead's own pane is closed; the CoS, human and other-farm panes are not
    survive(&herdr, &f);
    assert!(herdr.has_pane(&bystander));
    let ended: Option<i64> = conn
        .query_row(
            "SELECT ended_ms FROM agent WHERE name = 'proj-lead'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(ended, Some(9));
}

#[test]
fn prompt_archives_the_previous_report_and_waits_for_a_new_one() {
    let (runs, _c) = seeded("stale");
    let h = FakeHerdr::new();
    occupy(&h, "p1", "w");
    h.set_status("w", "idle");
    let tuning = Tuning {
        poll_ms: 5_000,
        retry_base_ms: 60_000,
        retry_max: 4,
    };
    let old = "Outcome: COMPLETE\n\n## Blockers\nold-round\n\n## Lessons\nnone\n";
    let new = "Outcome: COMPLETE\n\n## Blockers\nnew-round\n\n## Lessons\nnone\n";
    let report = runs.join("w.md");
    std::fs::write(&report, old).unwrap();

    // the worker answers only at t=20, after the prompt went out
    let clock = FakeClock {
        now: 0.into(),
        on_sleep: Box::new(|now| {
            if now >= 20 {
                std::fs::write(&report, new).unwrap();
            }
        }),
    };
    let args: Vec<String> = ["w", BRIEF].iter().map(|s| s.to_string()).collect();
    prompt::prompt_run(&h, &clock, &runs, &args, &tuning).unwrap();

    assert!(clock.now_sec() >= 20, "returned on the stale report");
    assert_eq!(std::fs::read_to_string(&report).unwrap(), new);
    let archived: Vec<_> = std::fs::read_dir(&runs)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with("w.") && n.ends_with(".md") && n != "w.md")
        .collect();
    assert_eq!(archived.len(), 1, "{archived:?}");
    assert_eq!(
        std::fs::read_to_string(runs.join(&archived[0])).unwrap(),
        old
    );
}

#[test]
fn pane_tail_block_has_one_header_and_the_last_40_lines() {
    let (runs, _c) = seeded("tail40");
    let h = FakeHerdr::new();
    occupy(&h, "p1", "w");
    let text: Vec<String> = (1..=60).map(|i| format!("line{i}")).collect();
    h.set_pane_text("p1", &text.join("\n"));
    let out = prompt::pane_tail_block(&h, &runs, "w");
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines[0], "WATCHDOG pane tail w");
    assert_eq!(lines.len(), 41, "{out}");
    assert_eq!((lines[1], lines[40]), ("line21", "line60"));
    assert_eq!(out.matches("WATCHDOG pane tail").count(), 1);
}

#[test]
fn pane_tail_reads_the_recorded_pane_when_the_core_died() {
    let (runs, conn) = seeded("tail8");
    let h = FakeHerdr::new();
    add_agent(&conn, "w", "peer", "tieude", None);
    conn.execute("UPDATE agent SET pane_id = 'p9' WHERE name = 'w'", [])
        .unwrap();
    h.set_pane_text("p9", "last words");
    // herdr has no agent `w` (exit 8), yet the recorded pane still reads
    let cfg_journal = |_: &str, _: serde_json::Value| {};
    let clock = FakeClock {
        now: 0.into(),
        on_sleep: Box::new(|_| {}),
    };
    assert_eq!(
        prompt::wait(&h, &clock, &wait_cfg(&runs, &cfg_journal, 7200)),
        Exit::CoreDied
    );
    assert_eq!(
        prompt::pane_tail_block(&h, &runs, "w"),
        "WATCHDOG pane tail w\nlast words"
    );
}

#[test]
fn pane_tail_failed_read_is_one_line_and_keeps_the_exit_code() {
    let (runs, conn) = seeded("tailfail");
    let h = FakeHerdr::new();
    add_agent(&conn, "w", "peer", "tieude", None);
    // no pane known anywhere: header plus one reason line
    let out = prompt::pane_tail_block(&h, &runs, "w");
    assert_eq!(out.lines().count(), 2, "{out}");
    assert!(out.contains("pane read failed"), "{out}");
    // a recorded pane herdr cannot find: the herdr error code is the reason
    conn.execute("UPDATE agent SET pane_id = 'gone' WHERE name = 'w'", [])
        .unwrap();
    let out = prompt::pane_tail_block(&h, &runs, "w");
    assert!(out.ends_with("pane read failed: pane_not_found"), "{out}");
    // the CLI path still exits 8
    let tuning = Tuning {
        poll_ms: 5_000,
        retry_base_ms: 60_000,
        retry_max: 4,
    };
    let clock = FakeClock {
        now: 0.into(),
        on_sleep: Box::new(|_| {}),
    };
    let args: Vec<String> = ["w", "--resume"].iter().map(|s| s.to_string()).collect();
    let e = prompt::prompt_run(&h, &clock, &runs, &args, &tuning).unwrap_err();
    assert_eq!(e.code, 8);
}

fn set_pilot_budget(runs: &Path, v: &str) {
    let c = db::open(&runs.join(db::path::DB_FILE)).unwrap();
    c.execute(
        "INSERT OR REPLACE INTO setting (key, value_json) VALUES ('timers.pilot_budget', ?1)",
        [format!("\"{v}\"")],
    )
    .unwrap();
}

#[test]
fn pilot_budget_rejects_a_prototype_brief_over_the_db_value() {
    let (runs, _c) = seeded("pilot");
    let proto = |min: u32| {
        BRIEF
            .replace("Playbook: feature", "Playbook: prototype")
            .replace("Time budget: 15 min", &format!("Time budget: {min} min"))
    };
    set_pilot_budget(&runs, "30m");
    let limit = prompt::pilot_budget_sec(&runs);
    assert_eq!(limit, Some(1800));
    assert!(prompt::check_pilot_budget(&proto(30), limit).is_ok());
    let e = prompt::check_pilot_budget(&proto(45), limit).unwrap_err();
    assert!(e.contains("45 min") && e.contains("30 min"), "{e}");
    // not a pilot: the same budget passes
    assert!(prompt::check_pilot_budget(BRIEF.replace("15 min", "45 min").as_str(), limit).is_ok());
    // the database, not team.toml, is the source: change it and the verdict follows
    set_pilot_budget(&runs, "60m");
    let limit = prompt::pilot_budget_sec(&runs);
    assert_eq!(limit, Some(3600));
    assert!(prompt::check_pilot_budget(&proto(45), limit).is_ok());
    // an unset key skips the check
    assert!(prompt::check_pilot_budget(&proto(999), None).is_ok());
}

#[test]
fn pilot_budget_blocks_the_prompt_before_anything_is_sent() {
    let (runs, _c) = seeded("pilotsend");
    set_pilot_budget(&runs, "10m");
    let h = FakeHerdr::new();
    occupy(&h, "p1", "w");
    let tuning = Tuning {
        poll_ms: 5_000,
        retry_base_ms: 60_000,
        retry_max: 4,
    };
    let clock = FakeClock {
        now: 0.into(),
        on_sleep: Box::new(|_| {}),
    };
    let brief = BRIEF.replace("Playbook: feature", "Playbook: prototype");
    let args: Vec<String> = vec!["w".into(), brief];
    let e = prompt::prompt_run(&h, &clock, &runs, &args, &tuning).unwrap_err();
    assert_eq!(e.code, 1);
    assert!(h.prompts().is_empty());
}
