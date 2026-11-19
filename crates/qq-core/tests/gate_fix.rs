//! Gate fix round after the T5 review (M1, M2, m1-m4, m6, R4 gate half).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use qq_core::authority::tier::{self, Action};
use qq_core::authority::{gate_evaluate_in, GateVerdict};
use qq_core::cli::Env;
use qq_core::daemon::{Config, Daemon, DaemonHandle};
use qq_core::herdr::{FakeHerdr, HerdrClient, Verb};
use qq_core::identity::peer::HerdrPeers;
use qq_core::testkit::TestWorkspace;
use rusqlite::params;
use serde_json::{json, Value};

#[derive(Clone, Copy, PartialEq)]
enum Who {
    Cos,
    Worker,
    Down,
}

struct Rig {
    ws: TestWorkspace,
    herdr: Arc<FakeHerdr>,
    daemon: Option<DaemonHandle>,
    /// Granted repo (full grant) and an unlisted one.
    a: PathBuf,
    b: PathBuf,
}

fn git(dir: &Path, args: &[&str]) {
    let o = std::process::Command::new("git")
        .args([
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(
        o.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&o.stderr)
    );
}

fn rig(tag: &str, who: Who) -> Rig {
    let ws = TestWorkspace::new(tag);
    ws.seal_herdr();
    let herdr = Arc::new(FakeHerdr::new());
    let daemon = if who == Who::Down {
        None
    } else {
        let mut d = Daemon::new(Config {
            runs_dir: ws.runs_dir(),
            herdr: herdr.clone() as Arc<dyn HerdrClient>,
        });
        d.require_peer(Arc::new(HerdrPeers::new(herdr.clone())));
        let h = d.spawn().unwrap();
        let pid = std::process::id();
        if who == Who::Cos {
            let p = herdr.seed_pane("c", Some("claude"), &[("grid_role", "cos")]);
            herdr.set_pane_pids(&p, pid, &[]);
        } else {
            let p = herdr.seed_pane("w", None, &[]);
            herdr.agent_start("w-1", "claude", &p, 0, &[]).unwrap();
            herdr.set_pane_pids(&p, pid, &[]);
        }
        Some(h)
    };
    let a = ws.root().join("repo-a");
    let b = ws.root().join("repo-b");
    for d in [&a, &b] {
        std::fs::create_dir_all(d).unwrap();
        git(d, &["init", "-q", "-b", "main"]);
    }
    let r = Rig {
        ws,
        herdr,
        daemon,
        a,
        b,
    };
    let c = qq_core::db::open(&r.ws.db_path()).unwrap();
    c.execute(
        "INSERT INTO grant_full (repo, granted_ms, granted_by) VALUES (?1, 1, 'cos')",
        params![tier::repo_key(&r.a)],
    )
    .unwrap();
    r
}

impl Drop for Rig {
    fn drop(&mut self) {
        if let Some(d) = self.daemon.take() {
            self.herdr.clear(Verb::PaneList);
            let p = self.herdr.seed_pane("stop", None, &[]);
            self.herdr
                .agent_start("stopper", "claude", &p, 0, &[])
                .unwrap();
            self.herdr.set_pane_pids(&p, std::process::id(), &[]);
            let _ = d.stop();
        }
    }
}

impl Rig {
    fn run(&self, cwd: &Path, tool: &str, input: Value) -> GateVerdict {
        let hook = json!({"tool_name": tool, "tool_input": input, "cwd": cwd});
        let env = Env {
            runs_dir: self.ws.runs_dir(),
        };
        gate_evaluate_in(&env, &hook.to_string())
    }
    fn bash_in(&self, cwd: &Path, cmd: &str) -> GateVerdict {
        self.run(cwd, "Bash", json!({ "command": cmd }))
    }
    /// Bash from the unlisted repo B (high tier unless the target says otherwise).
    fn bash(&self, cmd: &str) -> GateVerdict {
        self.bash_in(&self.b, cmd)
    }
    fn edit(&self, tool: &str, path: &Path) -> GateVerdict {
        self.run(&self.b, tool, json!({"file_path": path}))
    }
    fn root(&self) -> &Path {
        self.ws.root()
    }
}

fn ask(v: &GateVerdict) -> bool {
    matches!(v, GateVerdict::Ask(_))
}
fn block(v: &GateVerdict) -> bool {
    matches!(v, GateVerdict::Block(_))
}

// ---- M1: grant verbs are an approval-list hit (R14 consent) ----

const GRANT_CMDS: &[&str] = &[
    "quaqua call grant.full --args '{\"repo\":\"/x\"}'",
    "quaqua call grant.tier '{\"repo\":\"/x\",\"tier\":\"low\"}'",
    "quaqua call grant.lane '{}'",
    "quaqua --runs /tmp/r call grant.full '{}'",
    "/usr/local/bin/quaqua call grant.full '{}'",
    "env FOO=1 quaqua call grant.full '{}'",
    "bash -c \"quaqua call 'grant.full' '{}'\"",
    "quaqua call gra\"nt.\"full '{}'",
    "quaqua call GRANT.FULL '{}'",
    "quaqua call \"$VERB\" '{}'",
    "echo '{\"verb\":\"grant.full\"}' | socat - UNIX-CONNECT:runs/q.sock",
];

#[test]
fn ask_tier_grant_write_asks_cos_even_under_a_full_grant() {
    let r = rig("gf-grant-cos", Who::Cos);
    for cmd in GRANT_CMDS {
        assert!(ask(&r.bash_in(&r.a, cmd)), "cos must ask: {cmd}");
        assert!(ask(&r.bash(cmd)), "cos must ask: {cmd}");
    }
}

#[test]
fn ask_tier_grant_write_denies_everyone_else() {
    for (who, n) in [(Who::Worker, "w"), (Who::Down, "d")] {
        let r = rig(&format!("gf-grant-{n}"), who);
        for cmd in GRANT_CMDS {
            assert!(block(&r.bash(cmd)), "non-cos must deny: {cmd}");
        }
    }
}

#[test]
fn ask_tier_grant_write_reads_and_other_verbs_are_free() {
    let r = rig("gf-grant-ok", Who::Down);
    for ok in [
        "quaqua status",
        "quaqua call caller.lookup",
        "grep -rn grant.full src",
        "git commit -m 'doc grant.full'",
    ] {
        assert_eq!(r.bash(ok), GateVerdict::Allow, "{ok}");
    }
}

// ---- M2: the tier follows the command's target repo ----

#[test]
fn tier_source_wired_target_repo_beats_the_hook_cwd() {
    let r = rig("gf-target", Who::Cos);
    let a = r.a.display();
    let b = r.b.display();
    // cwd in the granted repo A, target elsewhere: must ask.
    for cmd in [
        format!("git -C {b} tag v1"),
        format!("git -C {a} -C {b} tag v1"),
        format!("git --git-dir={b}/.git tag v1"),
        format!("git --git-dir {b}/.git tag v1"),
        format!("git --work-tree {b} tag v1"),
        format!("GIT_DIR={b}/.git git tag v1"),
        format!("env GIT_DIR={b}/.git git tag v1"),
        format!("cd {b} && git tag v1"),
        format!("pushd {b}; git tag v1"),
        format!("(cd {b} && git tag v1)"),
        format!("bash -c 'cd {b} && git tag v1'"),
        format!("git -C {b} push origin --delete feat"),
        "gh -R owner/repo-b release create v1".to_string(),
        "gh --repo owner/repo-b release create v1".to_string(),
        "gh release create v1 --repo owner/repo-b".to_string(),
        "GH_REPO=owner/repo-b gh release create v1".to_string(),
        "git -C '$X' tag v1".to_string(),
        format!("git -C {}/missing tag v1", r.root().display()),
    ] {
        assert!(ask(&r.bash_in(&r.a, &cmd)), "must ask at A: {cmd}");
    }
    // The grant still works where it was given, by cwd or by target.
    assert_eq!(r.bash_in(&r.a, "git tag v1"), GateVerdict::Allow);
    assert_eq!(
        r.bash_in(&r.a, &format!("git -C {a} tag v1")),
        GateVerdict::Allow
    );
    assert_eq!(r.bash_in(&r.a, "git -C . tag v1"), GateVerdict::Allow);
    assert_eq!(
        r.bash_in(r.root(), &format!("git -C {a} tag v1")),
        GateVerdict::Allow
    );
    assert_eq!(
        r.bash_in(&r.a, "git -C ../repo-a tag v1"),
        GateVerdict::Allow
    );
    // A lone leading `cd <granted> &&` is read; every other spelling stays high.
    assert_eq!(
        r.bash_in(r.root(), &format!("cd {a} && git tag v1")),
        GateVerdict::Allow
    );
    for cmd in [
        format!("cd {a}; git tag v1"),
        format!("cd {a} || git tag v1"),
        format!("cd {a} && cd .. && git tag v1"),
    ] {
        assert!(ask(&r.bash_in(r.root(), &cmd)), "must ask: {cmd}");
    }
    // From B, a command aimed at B stays high.
    assert!(ask(&r.bash("git tag v1")));
}

// ---- m1: a branch delete is classed against the ref it deletes ----

#[test]
fn ask_tier_branch_delete_class_follows_the_deleted_ref() {
    let ws = TestWorkspace::new("gf-class");
    let up = ws.root().join("up");
    let cl = ws.root().join("cl");
    std::fs::create_dir_all(&up).unwrap();
    git(&up, &["init", "-q", "-b", "main"]);
    git(&up, &["commit", "-q", "--allow-empty", "-m", "a"]);
    git(&up, &["checkout", "-q", "-b", "x"]);
    git(&up, &["commit", "-q", "--allow-empty", "-m", "ahead"]);
    git(&up, &["checkout", "-q", "main"]);
    git(ws.root(), &["clone", "-q", up.to_str().unwrap(), "cl"]);
    // Local x sits on main (merged); origin/x is ahead (unmerged).
    git(&cl, &["branch", "x", "origin/main"]);
    assert_eq!(
        tier::branch_delete_class(&cl, "x", None),
        Action::BranchDeleteMerged
    );
    assert_eq!(
        tier::branch_delete_class(&cl, "x", Some("origin")),
        Action::BranchDeleteUnmerged
    );
    // Not a repo, or a missing ref, is never merged.
    assert_eq!(
        tier::branch_delete_class(ws.root(), "x", None),
        Action::BranchDeleteUnmerged
    );
    assert_eq!(
        tier::branch_delete_class(&cl, "nope", Some("origin")),
        Action::BranchDeleteUnmerged
    );
}

#[test]
fn branch_delete_class_unmerged_is_high_and_asks_at_every_tier() {
    use tier::{Decision, Tier};
    assert_eq!(
        tier::action_tier(Action::BranchDeleteUnmerged),
        Some(Tier::High)
    );
    assert_eq!(
        tier::action_tier(Action::BranchDeleteMerged),
        Some(Tier::Low)
    );
    for t in [Tier::Low, Tier::Medium, Tier::High] {
        assert_eq!(
            tier::decide(true, Action::BranchDeleteUnmerged, t, t),
            Decision::Ask,
            "unmerged at {t:?}"
        );
        assert_eq!(
            tier::decide(true, Action::BranchDeleteMerged, t, Tier::Low),
            if t == Tier::High {
                Decision::Ask
            } else {
                Decision::Allow
            },
            "merged at {t:?}"
        );
    }
}

impl Rig {
    fn set_tier(&self, repo: &Path, t: &str) {
        let c = qq_core::db::open(&self.ws.db_path()).unwrap();
        for (table, col) in [("repo_tier", "repo"), ("plan_lane", "lane")] {
            c.execute(
                &format!(
                    "INSERT OR REPLACE INTO {table} ({col}, tier, set_ms, set_by) VALUES (?1, ?2, 1, 'cos')"
                ),
                params![tier::repo_key(repo), t],
            )
            .unwrap();
        }
    }
}

/// A repo with `origin/main` at one commit, branch `merged` on it and branch
/// `feat` one commit ahead.
fn repo_with_branches(dir: &Path) {
    git(dir, &["commit", "-q", "--allow-empty", "-m", "a"]);
    git(dir, &["update-ref", "refs/remotes/origin/main", "HEAD"]);
    git(dir, &["branch", "merged"]);
    git(dir, &["checkout", "-q", "-b", "feat"]);
    git(dir, &["commit", "-q", "--allow-empty", "-m", "b"]);
    git(dir, &["checkout", "-q", "main"]);
}

#[test]
fn branch_delete_class_gate_unmerged_asks_at_every_tier_merged_follows_tier() {
    let r = rig("gf-bdc", Who::Cos);
    repo_with_branches(&r.b);
    for t in ["low", "medium", "high"] {
        r.set_tier(&r.b, t);
        assert!(ask(&r.bash("git branch -D feat")), "unmerged local at {t}");
        assert!(ask(&r.bash("git branch -d feat")), "unmerged -d at {t}");
        let merged = r.bash("git branch -d merged");
        if t == "high" {
            assert!(ask(&merged), "merged at high");
        } else {
            assert_eq!(merged, GateVerdict::Allow, "merged at {t}");
        }
    }
}

#[test]
fn branch_delete_class_gate_merge_base_is_checked_live() {
    let r = rig("gf-bdc-live", Who::Cos);
    repo_with_branches(&r.b);
    r.set_tier(&r.b, "low");
    assert!(ask(&r.bash("git branch -D feat")));
    // feat lands on origin/main: the same command is now a merged delete.
    git(&r.b, &["update-ref", "refs/remotes/origin/main", "feat"]);
    assert_eq!(r.bash("git branch -D feat"), GateVerdict::Allow);
    // and moving origin/main back makes it unmerged again.
    git(&r.b, &["update-ref", "refs/remotes/origin/main", "merged"]);
    assert!(ask(&r.bash("git branch -D feat")));
}

#[test]
fn branch_delete_class_gate_full_grant_is_the_only_exception() {
    let r = rig("gf-bdc-grant", Who::Cos);
    repo_with_branches(&r.a);
    // repo A holds the full grant: an unmerged delete passes alone.
    assert_eq!(r.bash_in(&r.a, "git branch -D feat"), GateVerdict::Allow);
    assert_eq!(r.bash_in(&r.a, "git branch -D merged"), GateVerdict::Allow);
    // aimed at B from A's cwd the grant does not follow.
    let b = r.b.display();
    repo_with_branches(&r.b);
    assert!(ask(&r.bash_in(&r.a, &format!("git -C {b} branch -D feat"))));
    // an unreadable target is unmerged and not granted
    assert!(ask(&r.bash_in(&r.a, "git -C '$X' branch -D feat")));
}

#[test]
fn branch_delete_class_gate_non_cos_still_denied() {
    let r = rig("gf-bdc-worker", Who::Worker);
    repo_with_branches(&r.a);
    assert!(block(&r.bash_in(&r.a, "git branch -D feat")));
    assert!(block(&r.bash_in(&r.a, "git branch -d merged")));
}

// ---- m2: parse misses ----

#[test]
fn ask_tier_tag_create_spellings_with_options() {
    let r = rig("gf-tag", Who::Cos);
    for cmd in [
        "git tag --sort=refname v9",
        "git tag --sort refname v9",
        "git tag --format='%(refname)' v9",
        "git tag --no-sort v9",
        "git tag -i v9",
        "git tag --color=never v9",
        "git tag -fa v9",
        "git tag -am msg v9",
        "git tag -F note.txt v9",
        "git tag --force v9 HEAD",
    ] {
        assert!(ask(&r.bash(cmd)), "{cmd}");
    }
    for ok in [
        "git tag --sort=refname",
        "git tag --sort refname",
        "git tag -l --sort=x 'v*'",
        "git tag --contains abc",
        "git tag --points-at HEAD",
        "git tag -n",
        "git tag -n5 'v*'",
        "git tag",
    ] {
        assert_eq!(r.bash(ok), GateVerdict::Allow, "{ok}");
    }
}

#[test]
fn ask_tier_branch_copy_onto_trunk() {
    {
        let r = rig("gf-copy", Who::Cos);
        for cmd in [
            "git branch -C feat main",
            "git branch -c feat master",
            "git branch --copy feat main",
            "git branch -fc feat main",
        ] {
            assert!(ask(&r.bash(cmd)), "{cmd}");
        }
        for ok in ["git branch -C feat feat2", "git branch -c feat feat2"] {
            assert_eq!(r.bash(ok), GateVerdict::Allow, "{ok}");
        }
    }
    let w = rig("gf-copy-w", Who::Worker);
    assert!(block(&w.bash("git branch -C feat main")));
}

#[test]
fn pane_input_gate_qq_db_writes_by_redirect_and_interpreter() {
    for who in [Who::Cos, Who::Worker, Who::Down] {
        let r = rig("gf-db", who);
        let db = r.ws.runs_dir().join("qq.db");
        for cmd in [
            format!("cat x > {}", db.display()),
            format!("echo hi >> {}", db.display()),
            format!("echo hi >{}", db.display()),
            format!("echo hi 2>&1 >| {}", db.display()),
            format!("cat x &> {}", db.display()),
            "cat x > runs/qq.db".to_string(),
            "python3 -c \"import sqlite3;sqlite3.connect('runs/qq.db')\"".to_string(),
            "node -e \"require('better-sqlite3')('runs/qq.db')\"".to_string(),
            "perl -e 'open F, \">runs/qq.db\"'".to_string(),
        ] {
            let v = r.bash(&cmd);
            if who == Who::Cos {
                assert!(ask(&v), "cos must ask: {cmd}");
            } else {
                assert!(block(&v), "non-cos must deny: {cmd}");
            }
        }
    }
    let r = rig("gf-db-ok", Who::Down);
    for ok in [
        "cat runs/qq.db > /tmp/copy",
        "echo hi > notes.txt",
        "python3 -c 'print(1)'",
        "git commit -m 'qq.db docs'",
    ] {
        assert_eq!(r.bash(ok), GateVerdict::Allow, "{ok}");
    }
}

// ---- m6: Bash writes to the live instruction files ----

#[test]
fn instruction_file_gate_bash_writes() {
    for name in ["AGENTS.md", "SOUL.md", "identity.md"] {
        for who in [Who::Cos, Who::Worker, Who::Down] {
            let r = rig("gf-ifb", who);
            let f = r.root().join(name);
            let f = f.display();
            for cmd in [
                format!("echo x > {f}"),
                format!("echo x >> {f}"),
                format!("echo x | tee {f}"),
                format!("echo x | tee -a {f}"),
                format!("sed -i s/a/b/ {f}"),
                format!("sed -i.bak s/a/b/ {f}"),
                format!("sed --in-place s/a/b/ {f}"),
                format!("cp /tmp/new {f}"),
                format!("cp -f /tmp/new {f}"),
                format!("mv /tmp/new {f}"),
                format!("install -m 644 /tmp/new {f}"),
                format!("truncate -s 0 {f}"),
                format!("dd if=/tmp/new of={f}"),
                format!("rm {f}"),
                format!("cd {} && echo x > {name}", r.root().display()),
                format!("bash -c 'echo x > {f}'"),
            ] {
                let v = r.bash(&cmd);
                if who == Who::Cos {
                    assert!(ask(&v), "cos must ask: {cmd}");
                } else {
                    assert!(block(&v), "non-cos must deny: {cmd}");
                }
            }
        }
    }
}

#[test]
fn instruction_file_gate_bash_reads_and_neighbours_are_free() {
    let r = rig("gf-ifb-ok", Who::Worker);
    let root = r.root().display();
    for ok in [
        format!("cat {root}/AGENTS.md"),
        format!("cp {root}/AGENTS.md /tmp/x"),
        format!("sed s/a/b/ {root}/AGENTS.md"),
        format!("echo x > {root}/crates/qq-core/templates/workspace/AGENTS.md"),
        format!("echo x > {root}/projects/p/AGENTS.md"),
        format!("echo x > {root}/notes.md"),
        "echo x > /tmp/elsewhere/SOUL.md".to_string(),
    ] {
        assert_eq!(r.bash(&ok), GateVerdict::Allow, "{ok}");
    }
}

// ---- R4 gate half: USER.md and memory/rules.md ----

#[test]
fn memory_path_gate_write_edit_deny_non_cos_allow_cos() {
    for who in [Who::Worker, Who::Down, Who::Cos] {
        let r = rig("gf-mem", who);
        for p in [r.root().join("USER.md"), r.root().join("memory/rules.md")] {
            for tool in ["Write", "Edit", "MultiEdit"] {
                let v = r.edit(tool, &p);
                assert!(block(&v), "{tool} {p:?}");
                if who == Who::Cos {
                    let GateVerdict::Block(m) = v else { panic!() };
                    for t in ["memory_add", "memory_replace", "memory_remove"] {
                        assert!(m.contains(t), "cos message names {t}: {m}");
                    }
                }
            }
        }
    }
}

#[test]
fn memory_path_gate_bash_writes() {
    for who in [Who::Worker, Who::Cos] {
        let r = rig("gf-memb", who);
        let root = r.root().display();
        for cmd in [
            format!("echo x > {root}/USER.md"),
            format!("echo x | tee {root}/memory/rules.md"),
            format!("sed -i s/a/b/ {root}/USER.md"),
            format!("cp /tmp/n {root}/memory/rules.md"),
            format!("mv /tmp/n {root}/USER.md"),
            format!("cd {root}/memory && echo x >> rules.md"),
        ] {
            let v = r.bash(&cmd);
            assert!(block(&v), "{cmd}");
            if who == Who::Cos {
                let GateVerdict::Block(m) = v else { panic!() };
                assert!(
                    m.contains("memory_add") && m.contains("memory_replace"),
                    "{m}"
                );
            }
        }
    }
}

#[test]
fn memory_path_gate_other_paths_and_reads_are_free() {
    let r = rig("gf-mem-ok", Who::Worker);
    let root = r.root().display();
    assert_eq!(
        r.edit("Write", &r.root().join("memory/notes.md")),
        GateVerdict::Allow
    );
    assert_eq!(
        r.edit("Write", Path::new("/tmp/x/USER.md")),
        GateVerdict::Allow
    );
    assert_eq!(
        r.edit("Write", &r.root().join("projects/p/memory/rules.md")),
        GateVerdict::Allow
    );
    for ok in [
        format!("cat {root}/USER.md"),
        format!("echo x > {root}/memory/notes.md"),
    ] {
        assert_eq!(r.bash(&ok), GateVerdict::Allow, "{ok}");
    }
    assert_eq!(
        r.run(&r.b, "Read", json!({"file_path": r.root().join("USER.md")})),
        GateVerdict::Allow
    );
}

// ---- m3: the fallback agrees with the binary ----

fn fallback_exit(cmd: &str) -> i32 {
    use std::io::Write;
    let script = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("templates/workspace/scripts/hooks/herdr-gate.sh");
    let json = json!({"tool_name": "Bash", "tool_input": {"command": cmd}});
    let mut child = std::process::Command::new("bash")
        .arg(&script)
        .env("QUAQUA_BIN", "qq-binary-that-does-not-exist")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(json.to_string().as_bytes())
        .unwrap();
    child.wait().unwrap().code().unwrap()
}

#[test]
fn hook_settings_route_fallback_global_flags_before_the_gh_subcommand() {
    for cmd in [
        "gh -R o/r pr merge 5",
        "gh --repo o/r pr merge 5 --squash",
        "gh -R o/r release create v1",
        "gh --repo=o/r release create v1 --notes x",
        "rm ~/quaqua/runs/qq.db",
        "cat x > ~/quaqua/runs/qq.db",
        "python3 -c \"import sqlite3;sqlite3.connect('runs/qq.db')\"",
        "tee runs/qq.db",
    ] {
        assert_eq!(fallback_exit(cmd), 2, "{cmd}");
    }
    for ok in [
        "gh -R o/r pr view 5",
        "gh pr list",
        "gh -R o/r release list",
        "cat runs/qq.db",
        "ls -l runs/qq.db",
    ] {
        assert_eq!(fallback_exit(ok), 0, "{ok}");
    }
}

// ---- m4: the R13 fence list ----

#[test]
fn fence_path_tier_covers_matcher_workflows_and_grant_sql() {
    use qq_core::daemon::pr_merge;
    for p in [
        "crates/qq-core/src/driver/claude.rs",
        ".github/workflows/ci.yml",
        ".github/CODEOWNERS",
        "crates/qq-core/src/db/sql/001_init.sql",
    ] {
        assert!(pr_merge::fence_path(p), "{p}");
    }
    for p in [
        "crates/qq-core/src/driver/codex.rs",
        "crates/qq-core/src/driver/claude_x.rs",
        ".githubx/a",
        "crates/qq-core/src/db/mod.rs",
    ] {
        assert!(!pr_merge::fence_path(p), "{p}");
    }
}
