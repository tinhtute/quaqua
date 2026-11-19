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

#[derive(Clone, Copy, PartialEq, Debug)]
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
    // Every `cd` spelling stays high, the lone leading one included (gatefix2 N1).
    for cmd in [
        format!("cd {a} && git tag v1"),
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

// ---- gate fix round 2: N1-N3, P1, m-new-1..3 ----

/// The CoS asks for each command; `cwd` picks the hook's directory.
fn gf2_asks(tag: &str, from_a: bool, cmds: impl Fn(&Rig) -> Vec<String>) {
    let r = rig(tag, Who::Cos);
    let cwd = if from_a { r.a.clone() } else { r.b.clone() };
    for cmd in cmds(&r) {
        assert!(ask(&r.bash_in(&cwd, &cmd)), "must ask: {cmd}");
    }
}

#[test]
fn gate_fix2_n1_lone_cd_is_high() {
    gf2_asks("gf2-n1-lone", false, |r| {
        vec![format!("cd {} && git tag v1", r.a.display())]
    });
}

#[test]
fn gate_fix2_n1_background_amp() {
    gf2_asks("gf2-n1-amp", false, |r| {
        vec![format!("cd {} & git tag v1", r.a.display())]
    });
}

#[test]
fn gate_fix2_n1_builtin_cd() {
    gf2_asks("gf2-n1-builtin", false, |r| {
        let (a, b) = (r.a.display(), r.b.display());
        vec![
            format!("cd {a} && builtin cd {b} && git tag v1"),
            format!("builtin cd {b} && git tag v1"),
        ]
    });
}

#[test]
fn gate_fix2_n1_command_cd() {
    gf2_asks("gf2-n1-command", false, |r| {
        let (a, b) = (r.a.display(), r.b.display());
        vec![
            format!("cd {a} && command cd {b} && git tag v1"),
            format!("command cd {b} && git tag v1"),
        ]
    });
}

#[test]
fn gate_fix2_n1_eval_cd() {
    gf2_asks("gf2-n1-eval", false, |r| {
        let (a, b) = (r.a.display(), r.b.display());
        vec![
            format!("cd {a} && eval cd {b} && git tag v1"),
            format!("cd {a} && eval 'cd {b}' && git tag v1"),
            format!("eval 'cd {b}' && git tag v1"),
        ]
    });
}

#[test]
fn gate_fix2_n1_env_chdir_after_cd() {
    gf2_asks("gf2-n1-envc", false, |r| {
        let (a, b) = (r.a.display(), r.b.display());
        vec![format!("cd {a} && env -C {b} git tag v1")]
    });
}

#[test]
fn gate_fix2_n2_relative_dash_c_after_cd() {
    gf2_asks("gf2-n2", true, |r| {
        let b = r.b.display();
        vec![
            format!("cd {b} && git -C . tag v1"),
            format!("cd {b} && git -C sub tag v1"),
        ]
    });
}

#[test]
fn gate_fix2_n3_env_dash_c() {
    gf2_asks("gf2-n3-envc", true, |r| {
        vec![format!("env -C {} git tag v1", r.b.display())]
    });
}

#[test]
fn gate_fix2_n3_env_chdir_joined() {
    gf2_asks("gf2-n3-envj", true, |r| {
        vec![format!("env --chdir={} git tag v1", r.b.display())]
    });
}

#[test]
fn gate_fix2_n3_env_chdir_separate() {
    gf2_asks("gf2-n3-envs", true, |r| {
        vec![format!("env --chdir {} git tag v1", r.b.display())]
    });
}

#[test]
fn gate_fix2_n3_sudo_dash_d() {
    gf2_asks("gf2-n3-sudo", true, |r| {
        let b = r.b.display();
        vec![
            format!("sudo -D {b} git tag v1"),
            format!("sudo --chdir {b} git tag v1"),
        ]
    });
}

#[test]
fn gate_fix2_n3_export_git_dir() {
    gf2_asks("gf2-n3-export", true, |r| {
        let b = r.b.display();
        vec![
            format!("export GIT_DIR={b}/.git && git tag v1"),
            format!("export GIT_DIR={b}/.git\ngit tag v1"),
            format!("GIT_DIR={b}/.git; git tag v1"),
            "export GH_REPO=o/b && gh release create v1".to_string(),
        ]
    });
}

fn gf2_blocks(tag: &str, cmds: &[&str]) {
    for who in [Who::Cos, Who::Worker, Who::Down] {
        let r = rig(&format!("{tag}-{}", who as u8), who);
        for cmd in cmds {
            assert!(block(&r.bash(cmd)), "must block: {cmd}");
        }
    }
}

#[test]
fn gate_fix2_p1_env_chdir_separate_value() {
    gf2_blocks(
        "gf2-p1-env",
        &[
            "env --chdir /tmp git push origin feat",
            "env --chdir /tmp herdr pane split p1",
            "env --unset FOO git push origin feat",
        ],
    );
}

#[test]
fn gate_fix2_p1_nice_adjustment() {
    gf2_blocks("gf2-p1-nice", &["nice --adjustment 5 git push origin feat"]);
}

#[test]
fn gate_fix2_p1_timeout_signal() {
    gf2_blocks(
        "gf2-p1-timeout",
        &[
            "timeout --signal KILL 5 git push origin feat",
            "timeout --kill-after 3 5 git push origin feat",
        ],
    );
}

#[test]
fn gate_fix2_p1_sudo_user() {
    gf2_blocks(
        "gf2-p1-sudo",
        &[
            "sudo --user x git push origin feat",
            "sudo --group g --user x git push origin HEAD:main",
        ],
    );
}

#[test]
fn gate_fix2_p1_xargs_max_args() {
    gf2_blocks("gf2-p1-xargs", &["xargs --max-args 1 git push origin feat"]);
}

#[test]
fn gate_fix2_p1_unknown_long_option_checks_both_readings() {
    gf2_blocks(
        "gf2-p1-unknown",
        &[
            "env --frobnicate /tmp git push origin feat",
            "nohup --frob x git push origin feat",
        ],
    );
    let r = rig("gf2-p1-ok", Who::Worker);
    for ok in [
        "env --chdir /tmp git status",
        "timeout --signal KILL 5 git log",
        "nice --adjustment 5 ls",
    ] {
        assert_eq!(r.bash(ok), GateVerdict::Allow, "{ok}");
    }
}

fn gf2_instr(tag: &str, cmd: impl Fn(&str, &str) -> String) {
    for who in [Who::Cos, Who::Worker, Who::Down] {
        let r = rig(&format!("{tag}-{}", who as u8), who);
        let root = r.root().display().to_string();
        let f = format!("{root}/AGENTS.md");
        let c = cmd(&root, &f);
        let v = r.bash(&c);
        if who == Who::Cos {
            assert!(ask(&v), "cos must ask: {c}");
        } else {
            assert!(block(&v), "non-cos must deny: {c}");
        }
    }
}

#[test]
fn gate_fix2_m1_ln() {
    gf2_instr("gf2-m1-ln", |_, f| format!("ln -sf /tmp/x {f}"));
}

#[test]
fn gate_fix2_m1_rsync() {
    gf2_instr("gf2-m1-rsync", |_, f| format!("rsync /tmp/x {f}"));
}

#[test]
fn gate_fix2_m1_perl_pi() {
    gf2_instr("gf2-m1-perl", |_, f| format!("perl -pi -e s/a/b/ {f}"));
}

#[test]
fn gate_fix2_m1_python() {
    gf2_instr("gf2-m1-py", |_, f| {
        format!("python3 -c \"open('{f}','w').write('x')\"")
    });
}

#[test]
fn gate_fix2_m1_git_checkout() {
    gf2_instr("gf2-m1-git", |root, _| {
        format!("git -C {root} checkout -- AGENTS.md")
    });
}

#[test]
fn gate_fix2_m1_reads_stay_free() {
    let r = rig("gf2-m1-ok", Who::Worker);
    let root = r.root().display();
    for ok in [
        format!("git -C {root} log -- AGENTS.md"),
        format!("git -C {root} diff AGENTS.md"),
        format!("cp {root}/AGENTS.md /tmp/x"),
        format!("bat {root}/AGENTS.md"),
        "git checkout -- AGENTS.md".to_string(),
    ] {
        assert_eq!(r.bash(&ok), GateVerdict::Allow, "{ok}");
    }
}

#[test]
fn gate_fix2_m2_fallback_branch_copy() {
    assert_eq!(fallback_exit("git branch -C feat main"), 2);
}

#[test]
fn gate_fix2_m2_fallback_tag_sort() {
    assert_eq!(fallback_exit("git tag --sort=refname v9"), 2);
    assert_eq!(fallback_exit("git tag --sort refname v9"), 2);
    assert_eq!(fallback_exit("git tag --sort=refname -l 'v*'"), 0);
    assert_eq!(fallback_exit("git tag -l"), 0);
}

#[test]
fn gate_fix2_m2_fallback_instruction_files() {
    for cmd in [
        "echo x > ~/quaqua/AGENTS.md",
        "sed -i s/a/b/ ~/quaqua/SOUL.md",
        "echo x > ~/quaqua/USER.md",
        "echo x >> ~/quaqua/memory/rules.md",
        "tee ~/quaqua/identity.md",
    ] {
        assert_eq!(fallback_exit(cmd), 2, "{cmd}");
    }
    for ok in [
        "cat ~/quaqua/AGENTS.md",
        "sed s/a/b/ ~/quaqua/SOUL.md",
        "grep x ~/quaqua/USER.md",
    ] {
        assert_eq!(fallback_exit(ok), 0, "{ok}");
    }
}

#[test]
fn gate_fix2_m2_fallback_grant_socat() {
    assert_eq!(
        fallback_exit("echo '{\"verb\":\"grant.full\"}' | socat - UNIX-CONNECT:runs/q.sock"),
        2
    );
    assert_eq!(fallback_exit("git commit -m 'grant.full docs'"), 0);
}

#[test]
fn gate_fix2_m3_call_through_command_substitution() {
    let r = rig("gf2-m3-sub", Who::Cos);
    for cmd in [
        "quaqua call \"$(cat /tmp/v)\" '{}'",
        "quaqua call `cat /tmp/v` '{}'",
    ] {
        assert!(ask(&r.bash_in(&r.a, cmd)), "{cmd}");
    }
    // One rig at a time: TestWorkspace holds a global lock for its lifetime.
    drop(r);
    let w = rig("gf2-m3-sub-w", Who::Worker);
    assert!(block(&w.bash("quaqua call \"$(cat /tmp/v)\" '{}'")));
}

#[test]
fn gate_fix2_m3_call_through_a_variable_command_word() {
    for who in [Who::Cos, Who::Worker] {
        let r = rig(&format!("gf2-m3-var-{}", who as u8), who);
        for cmd in [
            "Q=quaqua; $Q call grant.full '{}'",
            "${Q} call grant.tier '{}'",
            "\"$Q\" call grant.lane '{}'",
        ] {
            let v = r.bash_in(&r.a, cmd);
            if who == Who::Cos {
                assert!(ask(&v), "{cmd}");
            } else {
                assert!(block(&v), "{cmd}");
            }
        }
    }
    let r = rig("gf2-m3-var-ok", Who::Down);
    assert_eq!(r.bash("$Q status"), GateVerdict::Allow);
    assert_eq!(r.bash("$Q call caller.lookup"), GateVerdict::Allow);
}

#[test]
fn gate_fix2_m3_mail_body_naming_a_grant_is_free() {
    let r = rig("gf2-m3-mail", Who::Down);
    for ok in [
        "quaqua mail send --to x --body 'grant.full consent landed'",
        "git commit -m 'grant.tier' && quaqua status",
        "quaqua call task.spawn --args '{\"note\":\"see grant.full\"}'",
    ] {
        assert_eq!(r.bash(ok), GateVerdict::Allow, "{ok}");
    }
    // The real verb, and the xargs placeholder, still ask.
    drop(r);
    let c = rig("gf2-m3-mail-cos", Who::Cos);
    assert!(ask(&c.bash("printf grant.full | xargs -I{} quaqua call {}")));
    assert!(ask(&c.bash("quaqua call grant.full '{}' --note 'x'")));
}

// ---- gate fix round 2, review follow-ups ----

#[test]
fn gate_fix2_m3_substituted_command_word_and_stdin_interpreter() {
    for who in [Who::Cos, Who::Worker] {
        let r = rig(&format!("gf2-m3-subw-{}", who as u8), who);
        for cmd in [
            "$(which quaqua) call grant.full '{}'",
            "`which quaqua` call grant.tier '{}'",
            "python3 - <<'EOF'\nsend('grant.full')\nEOF",
            "echo grant.full | python3",
        ] {
            let v = r.bash_in(&r.a, cmd);
            if who == Who::Cos {
                assert!(ask(&v), "{cmd}");
            } else {
                assert!(block(&v), "{cmd}");
            }
        }
    }
    let r = rig("gf2-m3-subw-ok", Who::Down);
    for ok in [
        "python3 scripts/report.py --verb grant.full",
        "node --test tests/grant.full.test.js",
    ] {
        assert_eq!(r.bash(ok), GateVerdict::Allow, "{ok}");
    }
}

#[test]
fn gate_fix2_p1_short_flag_and_positional_wrappers() {
    gf2_blocks(
        "gf2-p1-short",
        &[
            "taskset -c 0 git push origin feat",
            "taskset 0x1 git push origin feat",
            "ionice -c 2 git push origin feat",
            "stdbuf -o L git push origin feat",
            "env -P /usr/bin git push origin feat",
            "chrt 10 git push origin feat",
            "env --a x --b x --c x --d x --e x --f x --g x --h x git push origin feat",
        ],
    );
}

fn fallback_exit_cwd(cmd: &str) -> i32 {
    use std::io::Write;
    let script = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("templates/workspace/scripts/hooks/herdr-gate.sh");
    let json = json!({"tool_name": "Bash", "cwd": "/Users/x/quaqua/node/python",
        "tool_input": {"command": cmd}});
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
fn gate_fix2_m2_fallback_ignores_cwd_and_word_fragments() {
    for ok in [
        "git commit -m 'grant.full docs'",
        "git add AGENTS.md",
        "grep x ~/quaqua/USER.md",
    ] {
        assert_eq!(fallback_exit_cwd(ok), 0, "{ok}");
    }
    assert_eq!(fallback_exit_cwd("sed -i s/a/b/ ~/quaqua/SOUL.md"), 2);
}

// ---- gatefix3 round 2: A. read-path tools that can write, B. old binary ----

/// Memory-file spellings that write or truncate through a tool the read path
/// used to wave through. `f` is the file as the command names it.
fn writer_spellings(f: &str) -> Vec<String> {
    vec![
        format!("sed -n 'w {f}' /dev/null"),
        format!("sed -n 'W {f}' /dev/null"),
        format!("sed 's/a/b/w {f}' /dev/null"),
        format!("sed -e 'w {f}' -n /dev/null"),
        format!("sed --expression='w {f}' /dev/null"),
        format!("sed '1e tee {f}' /dev/null"),
        format!("sed -I '' s/u/v/ {f}"),
        format!("sed -nI '' s/u/v/ {f}"),
        format!("uniq /dev/null {f}"),
        format!("uniq -c /dev/null {f}"),
        format!("uniq -f 1 /dev/null {f}"),
        format!("git grep -O'tee {f}' x"),
        format!("git grep --open-files-in-pager='tee {f}' x"),
        format!("git grep --open-files-in-pager 'tee {f}' x"),
        format!("git -c core.editor=true grep -Otee x -- {f}"),
    ]
}

#[test]
fn gate_fix3_a_read_path_writers_are_denied_for_workers_and_cos_memory() {
    for who in [Who::Worker, Who::Cos, Who::Down] {
        let r = rig("gf3-a", who);
        for name in ["USER.md", "memory/rules.md"] {
            let abs = r.root().join(name).display().to_string();
            for cmd in writer_spellings(&abs) {
                let v = r.bash(&cmd);
                assert!(block(&v), "{who:?} absolute: {cmd}");
            }
            for cmd in writer_spellings(name) {
                let v = r.bash_in(r.root(), &cmd);
                assert!(block(&v), "{who:?} relative: {cmd}");
            }
        }
    }
}

#[test]
fn gate_fix3_a_read_path_writers_on_instruction_files_ask_cos_deny_workers() {
    for who in [Who::Worker, Who::Cos] {
        let r = rig("gf3-a-i", who);
        for name in ["AGENTS.md", "SOUL.md", "identity.md"] {
            let abs = r.root().join(name).display().to_string();
            for cmd in writer_spellings(&abs) {
                let v = r.bash(&cmd);
                if who == Who::Cos {
                    assert!(ask(&v), "cos must ask: {cmd}");
                } else {
                    assert!(block(&v), "worker must be denied: {cmd}");
                }
            }
        }
    }
}

#[test]
fn gate_fix3_a_provable_reads_stay_free() {
    let r = rig("gf3-a-ok", Who::Worker);
    for name in ["USER.md", "memory/rules.md", "AGENTS.md"] {
        let f = r.root().join(name).display().to_string();
        for ok in [
            format!("sed -n 1,5p {f}"),
            format!("git diff -- {f}"),
            format!("git log -p -- {f}"),
            format!("awk 1 {f}"),
            format!("jq -R . {f}"),
            format!("sort {f}"),
            format!("uniq {f}"),
            format!("uniq -c {f}"),
            format!("uniq -f 1 {f}"),
            format!("sed 's/a/b/g' {f}"),
            format!("sed -E -n -e 's/u/v/p' {f}"),
            format!("sed -ne 3p {f}"),
            format!("sed -n '/new/p;$=' {f}"),
            format!("sed -s -n '1~2p;/x/!d' {f}"),
            format!("sed 'y/abc/xyz/;3q' {f}"),
            format!("git grep foo -- {f}"),
            format!("git grep -n -e foo -- {f}"),
        ] {
            assert_eq!(r.bash(&ok), GateVerdict::Allow, "{ok}");
        }
    }
}

/// Run the fallback-capable hook with `bin` as the installed binary.
fn hook_exit_with(bin: &Path, tool: &str, input: Value) -> i32 {
    use std::io::Write;
    let script = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("templates/workspace/scripts/hooks/herdr-gate.sh");
    let json = json!({"tool_name": tool, "tool_input": input, "cwd": "/tmp"});
    let mut child = std::process::Command::new("bash")
        .arg(&script)
        .env("QUAQUA_BIN", bin)
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

/// A stand-in `quaqua` that runs `body` (shell) with the hook JSON in `$IN`.
fn stub_bin(tag: &str, body: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let dir = std::env::temp_dir().join(format!("qq-gf3-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let p = dir.join("quaqua");
    std::fs::write(&p, format!("#!/bin/sh\nIN=$(cat)\n{body}\n")).unwrap();
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
    p
}

/// What the binary at main (e723967) does with every Write: no Write arm, so the
/// pane-spawn block, exit 2, for any tool name that is not Bash either.
const OLD_BIN: &str = "echo 'quaqua: blocked: spawn/move panes and workspaces only through the quaqua daemon (quaqua call task.spawn / grid verbs)' >&2; exit 2";

const EDIT_TOOLS: [(&str, &str); 4] = [
    ("Write", "file_path"),
    ("Edit", "file_path"),
    ("MultiEdit", "file_path"),
    ("NotebookEdit", "notebook_path"),
];

#[test]
fn gate_fix3_b_old_binary_keeps_write_and_edit_on_ordinary_files() {
    let bin = stub_bin("old-ok", OLD_BIN);
    for (tool, key) in EDIT_TOOLS {
        for p in [
            "/tmp/ws/projects/p/notes.md",
            "/tmp/ws/memory/notes.md",
            "/tmp/ws/src/main.rs",
        ] {
            assert_eq!(hook_exit_with(&bin, tool, json!({ key: p })), 0, "{tool} {p}");
        }
    }
}

#[test]
fn gate_fix3_b_old_binary_still_protects_memory_and_instruction_files() {
    let bin = stub_bin("old-prot", OLD_BIN);
    for (tool, key) in EDIT_TOOLS {
        for p in [
            "/tmp/ws/memory/rules.md",
            "/tmp/ws/USER.md",
            "/tmp/ws/AGENTS.md",
            "/tmp/ws/SOUL.md",
            "/tmp/ws/identity.md",
        ] {
            assert_eq!(hook_exit_with(&bin, tool, json!({ key: p })), 2, "{tool} {p}");
        }
    }
}

#[test]
fn gate_fix3_b_old_binary_bash_still_goes_to_the_binary() {
    let bin = stub_bin("old-bash", OLD_BIN);
    assert_eq!(hook_exit_with(&bin, "Bash", json!({"command": "echo hi"})), 2);
}

#[test]
fn gate_fix3_b_a_binary_with_the_write_gate_keeps_its_verdict() {
    // Knows Write: allows ordinary paths, blocks one named path with its own reason.
    let bin = stub_bin(
        "new",
        "case \"$IN\" in *blocked-by-new*) echo 'new gate says no' >&2; exit 2;; esac; exit 0",
    );
    for (tool, key) in EDIT_TOOLS {
        assert_eq!(
            hook_exit_with(&bin, tool, json!({ key: "/tmp/ws/blocked-by-new.txt" })),
            2,
            "{tool}: the binary's block must stand"
        );
        assert_eq!(
            hook_exit_with(&bin, tool, json!({ key: "/tmp/ws/ok.txt" })),
            0,
            "{tool}"
        );
    }
}

// ---- gate allowlist (sude-gateallow): known spellings are the fast path ----
//
// Review 3 findings 1, 3, 4, 10, then a held-out set no review listed. The
// held-out cases are the test of the premise "the gate can enumerate the unsafe
// spellings": they must pass with no case-by-case additions.

/// Hard blocks (push, pane spawn) end BLOCK for every role, like their siblings.
#[test]
fn gate_fix4_review3_clustered_wrapper_flags_block() {
    gf2_blocks(
        "gf4-r3-wrap",
        &[
            "env -iC /tmp git push origin feat",
            "env -iu FOO git push origin feat",
            "sudo -Eu root git push origin feat",
            "sudo -nu root git push origin feat",
            "xargs -tn 1 git push origin feat",
            "timeout -vs KILL 5 git push origin feat",
            "env -iS 'git push origin feat'",
            "sudo -Eu root herdr pane split p1",
        ],
    );
}

#[test]
fn gate_fix4_review3_unlisted_wrappers_block() {
    gf2_blocks(
        "gf4-r3-unl",
        &[
            "caffeinate -i git push origin feat",
            "script -q /dev/null git push origin feat",
            "doas -u root git push origin feat",
            "unbuffer git push origin feat",
            "flock /tmp/l git push origin feat",
            "watch -n 5 git push origin feat",
            "parallel -j1 git push origin ::: feat",
        ],
    );
}

#[test]
fn gate_fix4_review3_clustered_flags_keep_the_tier() {
    gf2_asks("gf4-r3-tier", true, |r| {
        let b = r.b.display();
        vec![
            format!("env -iC {b} git tag v1"),
            format!("sudo -nD {b} git tag v1"),
        ]
    });
}

/// Writers through a reader on `f`, as the command names it.
fn gf4_reader_writers(f: &str) -> Vec<String> {
    vec![
        // review 3, finding 3 and 10
        format!("sed -n 'r /dev/null\\\nw {f}' in.txt"),
        format!("xxd in.txt {f}"),
        format!("xxd -r in.hex {f}"),
        format!("rg --pre tee x {f}"),
        format!("bat --pager='tee {f}' x"),
        format!("sort -o{f} in.txt"),
        // held-out: not listed by any review
        format!("sed -n 'R /dev/null\\\nW {f}' in.txt"),
        format!("sed -n '1{{r /dev/null\\\nw {f}\n}}' in.txt"),
        format!("xxd -p -r in.hex {f}"),
        format!("xxd -ps in.txt {f}"),
        format!("xxd -i in.txt {f}"),
        format!("rg -n --pre=tee x {f}"),
        format!("rg --pre-glob '*' --pre=tee x {f}"),
        format!("bat --paging=always --pager 'tee {f}' x"),
        format!("batcat -P --pager='tee {f}' x"),
        format!("less -o{f} in.txt"),
        format!("less --log-file={f} in.txt"),
        format!("sort -ruo{f} in.txt"),
        format!("sort --compress-program 'tee {f}' in.txt"),
        format!("awk '{{print | \"tee {f}\"}}' in.txt"),
        format!("awk 'END{{printf \"x\" | \"tee {f}\"}}' in.txt"),
        format!("awk '{{ \"tee {f}\" | getline }}' in.txt"),
        format!("gawk -iinplace '{{print}}' {f}"),
    ]
}

#[test]
fn gate_fix4_readers_that_write_memory_files_block_every_role() {
    for who in [Who::Worker, Who::Cos, Who::Down] {
        let r = rig("gf4-rd-mem", who);
        for name in ["USER.md", "memory/rules.md"] {
            let abs = r.root().join(name).display().to_string();
            for cmd in gf4_reader_writers(&abs) {
                assert!(block(&r.bash(&cmd)), "{who:?} absolute: {cmd}");
            }
            for cmd in gf4_reader_writers(name) {
                assert!(block(&r.bash_in(r.root(), &cmd)), "{who:?} relative: {cmd}");
            }
        }
    }
}

#[test]
fn gate_fix4_readers_that_write_instruction_files_ask_cos_deny_workers() {
    for who in [Who::Worker, Who::Cos] {
        let r = rig("gf4-rd-ins", who);
        for name in ["AGENTS.md", "SOUL.md"] {
            let abs = r.root().join(name).display().to_string();
            for cmd in gf4_reader_writers(&abs) {
                let v = r.bash(&cmd);
                if who == Who::Cos {
                    assert!(ask(&v), "cos must ask: {cmd}");
                } else {
                    assert!(block(&v), "worker must be denied: {cmd}");
                }
            }
        }
    }
}

/// Held-out: other clustered flags and other wrappers, no review listed them.
#[test]
fn gate_fix4_heldout_wrappers_block() {
    gf2_blocks(
        "gf4-ho-wrap",
        &[
            "env -0iu FOO git push origin feat",
            "env -vi -u FOO git push origin feat",
            "sudo -HEu root git push origin feat",
            "sudo -nkH -g wheel git push origin feat",
            "xargs -rtn1 git push origin feat",
            "xargs -0rI{} git push origin feat",
            "timeout -pvk 3 9 git push origin feat",
            "nice -n5 git push origin feat",
            "nice -5 git push origin feat",
            "ionice -tc3 git push origin feat",
            "stdbuf -oL -eL git push origin feat",
            "env -iC /tmp herdr workspace create",
            "sudo -HEu root herdr pane close p1",
            "ssh host git push origin feat",
            "chroot / git push origin feat",
            "lockf /tmp/l git push origin feat",
            "runuser -u x git push origin feat",
            "systemd-run --user git push origin feat",
            "nsenter -t 1 git push origin feat",
            "unshare -r git push origin feat",
            "proxychains4 -q git push origin feat",
            "direnv exec . git push origin feat",
            "mise exec -- git push origin feat",
            "op run -- git push origin feat",
            "lockf /tmp/l herdr pane split p1",
            "direnv exec . herdr pane close p1",
            "watch -n1 'git push origin feat'",
            "flock -n /tmp/l -c 'git push origin feat'",
            "script -qc 'git push origin feat' /dev/null",
            "ssh host 'git push origin feat'",
            "tmux new-session -d 'git push origin feat'",
            "su -c 'git push origin feat' root",
            "sg wheel -c 'git push origin feat'",
        ],
    );
}

#[test]
fn gate_fix4_heldout_tier_escapes_ask() {
    gf2_asks("gf4-ho-tier", true, |r| {
        let b = r.b.display();
        vec![
            format!("env -vC {b} git tag v1"),
            format!("env -0C {b} git tag v1"),
            format!("sudo -HD {b} git tag v1"),
            format!("sudo -nkD {b} git tag v1"),
            format!("env -iC {b} gh release create v1"),
            format!("direnv exec {b} git tag v1"),
            format!("lockf /tmp/l git tag v1"),
            format!("chroot {b} git tag v1"),
        ]
    });
}

/// The strict path must not make ordinary work ask or block.
#[test]
fn gate_fix4_ordinary_commands_stay_free() {
    let r = rig("gf4-free", Who::Worker);
    for name in ["USER.md", "memory/rules.md", "AGENTS.md"] {
        let f = r.root().join(name).display().to_string();
        for ok in [
            format!("rg -n foo {f}"),
            format!("rg -il foo -g '*.md' {f}"),
            format!("bat -n {f}"),
            format!("bat --style=plain --paging=never {f}"),
            format!("sort -u {f}"),
            format!("sort -k2,2 -t, -r {f}"),
            format!("diff -u /tmp/a {f}"),
            format!("awk '{{print $1}}' {f}"),
            format!("awk -F: '{{print $2}}' {f}"),
            format!("sed -n '/a/{{p;q}}' {f}"),
            format!("cat {f}"),
            format!("ls -la {f}"),
            format!("head -5 {f}"),
            format!("wc -l {f}"),
            format!("grep -rn foo {f}"),
            format!("hexdump -C {f}"),
        ] {
            assert_eq!(r.bash(&ok), GateVerdict::Allow, "{ok}");
        }
    }
    for ok in [
        "env -i ls",
        "env -iu FOO ls -la",
        "sudo -n git status",
        "sudo -Eu root ls",
        "timeout -vs KILL 5 git log",
        "xargs -tn 1 echo git push",
        "echo git push",
        "printf 'git push\\n'",
        "grep -rn git push docs",
        "rg -n 'git push' docs",
        "ssh host ls",
        "caffeinate -i sleep 1",
        "flock /tmp/l ls",
        "watch -n 5 ls",
        "nice -n5 cargo build",
    ] {
        assert_eq!(r.bash(ok), GateVerdict::Allow, "{ok}");
    }
}
