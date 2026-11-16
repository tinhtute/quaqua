//! The verbs ported from the old engine: playbook, memory hook, transcripts
//! tail, identity heal, inbox, init --update, and the SessionStart hook.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use qq_core::cli::{Env, Registry};
use qq_core::daemon::{Config, Daemon, DaemonHandle};
use qq_core::herdr::FakeHerdr;
use qq_core::identity::{self, PaneOps};
use qq_core::init;
use qq_core::memory;
use qq_core::playbook;
use qq_core::rpc::{self, Request};
use qq_core::testkit::TestWorkspace;
use qq_core::transcripts;
use serde_json::{json, Value};

/// A herdr pane that remembers the tokens written to it.
struct FakePane {
    session: Option<String>,
    tokens: RefCell<BTreeMap<String, String>>,
    writes: RefCell<usize>,
}

impl FakePane {
    fn new(session: Option<&str>, tokens: &[(&str, &str)]) -> Self {
        FakePane {
            session: session.map(String::from),
            tokens: RefCell::new(
                tokens
                    .iter()
                    .map(|(k, v)| (k.to_string(), v.to_string()))
                    .collect(),
            ),
            writes: RefCell::new(0),
        }
    }
    fn token(&self, k: &str) -> Option<String> {
        self.tokens.borrow().get(k).cloned()
    }
}

impl PaneOps for FakePane {
    fn pane_get(&self, _: &str) -> Result<Value, String> {
        let mut pane = json!({ "tokens": *self.tokens.borrow() });
        if let Some(s) = &self.session {
            pane["agent_session"] = json!({ "value": s });
        }
        Ok(json!({ "result": { "pane": pane } }))
    }
    fn pane_report_metadata(
        &self,
        _: &str,
        source: &str,
        tokens: &[(&str, &str)],
    ) -> Result<(), String> {
        assert_eq!(source, identity::TOKEN_SOURCE);
        *self.writes.borrow_mut() += 1;
        for (k, v) in tokens {
            self.tokens
                .borrow_mut()
                .insert(k.to_string(), v.to_string());
        }
        Ok(())
    }
}

fn args(a: &[&str]) -> Vec<String> {
    a.iter().map(|s| s.to_string()).collect()
}

fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

/// A lead -> peer mail edge and two agents, enough for `mail.send`.
fn seed_agents(ws: &TestWorkspace) {
    let conn = qq_core::db::open(&ws.db_path()).expect("open qq db");
    conn.execute(
        "INSERT OR IGNORE INTO topology_edge (from_role, to_role, edge) VALUES ('lead', 'peer', 'mail')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO project (slug, created_ms) VALUES ('p1', 0)",
        [],
    )
    .unwrap();
    for (name, role) in [("lead", "lead"), ("peer-a", "peer")] {
        conn.execute(
            "INSERT INTO agent (name, role, project_id, started_ms) VALUES (?1, ?2, 1, 1)",
            rusqlite::params![name, role],
        )
        .unwrap();
    }
}

fn start(ws: &TestWorkspace) -> DaemonHandle {
    seed_agents(ws);
    let mut d = Daemon::new(Config {
        runs_dir: ws.runs_dir(),
        herdr: Arc::new(FakeHerdr::new()),
    });
    qq_core::bus::install(&mut Registry::new(), &mut d).expect("bus installs");
    d.spawn().expect("daemon starts")
}

fn send(ws: &TestWorkspace, to: &str, body: &str) {
    let mut req = Request::new("mail.send", json!({"to": to, "body": body, "kind": "task"}));
    req.caller = Some("lead".into());
    rpc::call(&rpc::socket_path(&ws.runs_dir()), &req)
        .expect("transport")
        .into_result()
        .expect("mail.send");
}

#[test]
fn ported_verbs_answer() {
    let ws = TestWorkspace::new("ported-verbs");
    let root = ws.root();
    init::run(root, false, false).expect("init");

    // playbook list / show read the workspace the template just wrote.
    let (pb, pr) = (root.join("playbooks"), root.join("principles"));
    let list = playbook::run_list(&pb, &pr).expect("list");
    assert!(list.contains("feature"), "{list}");
    assert!(!playbook::run_show("feature", &pb, &pr)
        .expect("show")
        .trim()
        .is_empty());
    assert!(playbook::run_show("no-such-playbook", &pb, &pr).is_err());

    // memory hook pre: a write that breaks the rules.md cap is denied; a short one is not.
    let rules = root.join("memory/rules.md");
    let call = |content: &str| {
        json!({"tool_name": "Write", "tool_input": {"file_path": rules, "content": content}})
            .to_string()
    };
    let denied = memory::pre_decision(&call(&"x".repeat(memory::RULES_CAP + 1))).expect("deny");
    assert_eq!(denied["hookSpecificOutput"]["permissionDecision"], "deny");
    assert!(memory::pre_decision(&call("- one short rule")).is_none());
    assert!(
        memory::pre_decision("not json").is_none(),
        "hooks fail open"
    );

    // transcripts tail: the last user messages of the main conversation only.
    let projects = root.join("projects-corpus");
    let line = |ty: &str, content: Value, side: bool| {
        json!({"type": ty, "isSidechain": side, "message": {"role": ty, "content": content}})
            .to_string()
    };
    let jsonl = [
        line("user", json!("first ask"), false),
        line(
            "assistant",
            json!([{"type": "text", "text": "an answer"}]),
            false,
        ),
        line(
            "user",
            json!([{"type": "tool_result", "content": "x"}]),
            false,
        ),
        line("user", json!("a subagent line"), true),
        line("user", json!("second   ask\nwrapped"), false),
        line(
            "user",
            json!("<system-reminder>injected</system-reminder>"),
            false,
        ),
        "{truncated".to_string(),
    ]
    .join("\n");
    write(&projects.join("proj/sess-1.jsonl"), &jsonl);
    let got = transcripts::tail("sess-1", 1, Some(&projects)).expect("tail");
    assert_eq!(got, "second ask wrapped");
    let got = transcripts::tail("sess-1", 10, Some(&projects)).expect("tail");
    assert_eq!(got, "first ask\nsecond ask wrapped");
    assert!(transcripts::tail("missing", 1, Some(&projects)).is_err());

    // identity heal: tags the recorded pane once, then has nothing to do.
    write(
        &root.join("runs/cos.identity.json"),
        &json!({"agent_session": {"value": "sess-cos"}, "pane_id": "p9"}).to_string(),
    );
    let pane = FakePane::new(Some("sess-cos"), &[]);
    let first = identity::heal(&pane, &ws.runs_dir()).expect("heal");
    assert_eq!(first.len(), 1, "{first:?}");
    assert_eq!(pane.token("grid_role").as_deref(), Some("cos"));
    assert!(identity::heal(&pane, &ws.runs_dir())
        .expect("heal again")
        .is_empty());
    assert_eq!(*pane.writes.borrow(), 1);
    let other = FakePane::new(Some("another-session"), &[]);
    assert!(identity::heal(&other, &ws.runs_dir()).unwrap().is_empty());
    assert_eq!(
        *other.writes.borrow(),
        0,
        "a pane hosting another session is left alone"
    );

    // inbox: a digest, then silence; --peek keeps the mail.
    let daemon = start(&ws);
    let env = Env {
        runs_dir: ws.runs_dir(),
    };
    let inbox = |a: &[&str]| memory::inbox_cmd(&env, &args(a)).expect("inbox");
    assert_eq!(inbox(&["--for", "peer-a"]), "", "no mail, no output");
    send(&ws, "peer-a", "build T45\nsecond line");
    send(&ws, "peer-a", "and T46");
    let peeked = inbox(&["--for", "peer-a", "--peek"]);
    assert_eq!(peeked.lines().count(), 2, "{peeked}");
    assert!(peeked
        .lines()
        .next()
        .unwrap()
        .contains("lead [task] build T45"));
    assert!(!peeked.contains("second line"), "only the first body line");
    assert_eq!(
        inbox(&["--for", "peer-a"]),
        peeked,
        "a peek leaves it for the next read"
    );
    assert_eq!(inbox(&["--for", "peer-a"]), "", "read once, gone");
    send(&ws, "peer-a", "later");
    let hook: Value =
        serde_json::from_str(&inbox(&["--for", "peer-a", "--format", "hook-ups"])).unwrap();
    assert_eq!(
        hook["hookSpecificOutput"]["hookEventName"],
        "UserPromptSubmit"
    );
    assert!(hook["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap()
        .contains("later"));
    send(&ws, "peer-a", "stop-time");
    let stop: Value =
        serde_json::from_str(&inbox(&["--for", "peer-a", "--format", "hook-stop"])).unwrap();
    assert_eq!(stop["decision"], "block");
    assert_eq!(
        memory::inbox_cmd(&env, &args(&["--format", "xml"]))
            .unwrap_err()
            .code,
        2
    );
    daemon.stop().unwrap();
}

#[test]
fn init_update_writes_templates() {
    let ws = TestWorkspace::new("init-update");
    let root = ws.root().join("fresh");

    // A fresh directory gets every template file, executables included.
    let (report, conflicts) = init::run(&root, true, true).expect("dry run");
    assert_eq!(conflicts, 0);
    assert!(report.contains("dry run"), "{report}");
    assert!(!root.join("AGENTS.md").exists(), "dry run writes nothing");
    let (_, conflicts) = init::run(&root, true, false).expect("update");
    assert_eq!(conflicts, 0);
    assert!(root.join("AGENTS.md").is_file());
    assert!(root.join(".quaqua/template.lock").is_file());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(root.join("scripts/hooks/herdr-gate.sh"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o111, 0o111, "the gate script stays executable");
    }

    // Idempotent: a second run writes nothing.
    let (report, conflicts) = init::run(&root, true, false).expect("again");
    assert_eq!((conflicts, report.as_str()), (0, "workspace up to date"));

    // A file the user changed is never overwritten: the template goes to pending.
    let agents = root.join("AGENTS.md");
    let template = std::fs::read(&agents).unwrap();
    write(&agents, "my own rules\n");
    let (report, conflicts) = init::run(&root, true, false).expect("conflict");
    assert_eq!(conflicts, 1, "{report}");
    assert_eq!(std::fs::read_to_string(&agents).unwrap(), "my own rules\n");
    assert_eq!(
        std::fs::read(root.join(".quaqua/pending/AGENTS.md")).unwrap(),
        template
    );

    // A file untouched since the lock, whose template moved on, is overwritten.
    let team = root.join("team.toml");
    let current = std::fs::read(&team).unwrap();
    write(&team, "# older template\n");
    let lock_path = root.join(".quaqua/template.lock");
    let mut lock: Value =
        serde_json::from_str(&std::fs::read_to_string(&lock_path).unwrap()).unwrap();
    lock["files"]["team.toml"]["md5"] = json!(init::md5_hex(b"# older template\n"));
    write(&lock_path, &lock.to_string());
    let (report, conflicts) = init::run(&root, true, false).expect("overwrite");
    assert_eq!(conflicts, 1, "only AGENTS.md still conflicts: {report}");
    assert!(report.contains("overwrite team.toml"), "{report}");
    assert_eq!(std::fs::read(&team).unwrap(), current);

    // Plain init over a workspace that predates the template keeps its files.
    let old = ws.root().join("old");
    write(&old.join("AGENTS.md"), "predates\n");
    init::run(&old, false, false).expect("init old");
    assert_eq!(
        std::fs::read_to_string(old.join("AGENTS.md")).unwrap(),
        "predates\n"
    );
    assert!(old.join("memory").is_dir() && old.join("playbooks").is_dir());
    let (_, conflicts) = init::run(&old, true, false).expect("update old");
    assert_eq!(
        conflicts, 1,
        "the first --update reports it instead of overwriting"
    );
}

#[test]
fn session_start_rereads_and_retags() {
    let ws = TestWorkspace::new("session-start");
    let root = ws.root();
    write(&root.join("AGENTS.md"), "rules v1\n");
    write(&root.join("team.toml"), "");
    write(&root.join("memory/rules.md"), "- r\n");
    write(&root.join("projects/INDEX.md"), "idx\n");
    write(&root.join("reports/audit-2026-10-01.md"), "old\n");
    write(&root.join("reports/audit-2026-11-01.md"), "new\n");
    let input = |sid: &str, source: &str, cwd: &Path| {
        json!({"session_id": sid, "source": source, "cwd": cwd}).to_string()
    };
    let baseline = root.join("runs/.sessions/s1.json");
    let pane = FakePane::new(None, &[]);

    // After a compaction: the re-read list, a baseline, and the CoS tokens.
    let out = memory::session_start(root, &input("s1", "compact", root), &pane, Some("p1"))
        .expect("re-read list");
    for want in [
        "AGENTS.md",
        "memory/rules.md",
        "projects/INDEX.md",
        "audit-2026-11-01.md",
    ] {
        assert!(out.contains(want), "{want} missing from:\n{out}");
    }
    assert!(
        !out.contains("audit-2026-10-01.md"),
        "only the newest audit"
    );
    assert!(baseline.is_file());
    assert_eq!(pane.token("grid_role").as_deref(), Some("cos"));
    assert_eq!(pane.token("grid_owner").as_deref(), Some("cos"));
    assert_eq!(
        pane.token("grid_farm").as_deref(),
        Some(identity::farm_of(root).as_str())
    );
    assert_eq!(*pane.writes.borrow(), 1);

    // Idempotent: the pane is already tagged, so nothing more is written.
    memory::session_start(root, &input("s1", "compact", root), &pane, Some("p1"));
    assert_eq!(*pane.writes.borrow(), 1);

    // A fresh start prints nothing; a resume keeps the baseline it had.
    assert!(
        memory::session_start(root, &input("s1", "startup", root), &pane, Some("p1")).is_none()
    );
    write(&root.join("AGENTS.md"), "rules v2\n");
    let before = std::fs::read_to_string(&baseline).unwrap();
    memory::session_start(root, &input("s1", "resume", root), &pane, Some("p1"));
    assert_eq!(std::fs::read_to_string(&baseline).unwrap(), before);
    memory::session_start(root, &input("s1", "startup", root), &pane, Some("p1"));
    assert_ne!(
        std::fs::read_to_string(&baseline).unwrap(),
        before,
        "a new start re-baselines"
    );

    // Never re-tag a pane that carries another role, or a session outside the workspace.
    let worker = FakePane::new(None, &[("grid_role", "lead")]);
    memory::session_start(root, &input("s2", "startup", root), &worker, Some("p2"));
    assert_eq!(*worker.writes.borrow(), 0);
    let elsewhere = FakePane::new(None, &[]);
    memory::session_start(
        root,
        &input("s3", "startup", &root.join("memory")),
        &elsewhere,
        Some("p3"),
    );
    memory::session_start(root, &input("s4", "startup", root), &elsewhere, None);
    assert_eq!(*elsewhere.writes.borrow(), 0);

    // Hooks fail open: garbage in, nothing out.
    assert!(memory::session_start(root, "not json", &elsewhere, None).is_none());
}

/// Runs a workspace's git with a fixed identity, failing the test on error.
fn git(dir: &Path, a: &[&str]) -> String {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
        .args(a)
        .output()
        .expect("git runs");
    assert!(
        out.status.success(),
        "git {a:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

#[test]
fn init_seeds_hooks_and_cos_identity() {
    let ws = TestWorkspace::new("init-seeds");
    let root = ws.root();
    git(root, &["init", "-q"]);
    write(&root.join("USER.md"), "mine\n");

    let (report, conflicts) = init::run(root, false, false).expect("init");
    assert_eq!(conflicts, 0);
    // Seeds: written when absent, never overwritten.
    for f in [
        "memory/rules.md",
        "projects/INDEX.md",
        "projects/_template.md",
    ] {
        assert!(
            !std::fs::read_to_string(root.join(f)).unwrap().is_empty(),
            "{f}"
        );
    }
    assert_eq!(
        std::fs::read_to_string(root.join("USER.md")).unwrap(),
        "mine\n"
    );
    assert!(report.contains("seed memory/rules.md"), "{report}");
    write(&root.join("memory/rules.md"), "- edited\n");
    init::run(root, false, false).expect("rerun");
    assert_eq!(
        std::fs::read_to_string(root.join("memory/rules.md")).unwrap(),
        "- edited\n"
    );

    // The pre-commit gate rejects an added machine path and passes a clean line.
    let hooks = git(root, &["config", "--get", "core.hooksPath"]);
    assert!(hooks.ends_with("quaqua-hooks"), "{hooks}");
    let bad = format!("see {}\n", ["/home", "someone", "x"].join("/"));
    write(&root.join("note.md"), &bad);
    git(root, &["add", "note.md"]);
    let rejected = std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args([
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@example.com",
            "commit",
            "-qm",
            "x",
        ])
        .output()
        .unwrap();
    assert!(!rejected.status.success(), "machine path must be rejected");
    write(&root.join("note.md"), "clean\n");
    git(root, &["add", "note.md"]);
    git(root, &["commit", "-qm", "ok"]);

    // `--git-hooks` alone wires a second repo and refuses a foreign hooksPath.
    let env = Env {
        runs_dir: ws.runs_dir(),
    };
    let other = root.join("other");
    std::fs::create_dir_all(&other).unwrap();
    git(&other, &["init", "-q"]);
    let ok = init::init_cmd(&env, &args(&["--git-hooks", other.to_str().unwrap()])).unwrap();
    assert!(ok.contains("git hooks"), "{ok}");
    assert!(!other.join("projects").exists(), "no workspace dirs");
    git(&other, &["config", "core.hooksPath", "/elsewhere"]);
    let err = init::init_cmd(&env, &args(&["--git-hooks", other.to_str().unwrap()])).unwrap_err();
    assert_eq!(err.code, 1);
    assert_eq!(
        git(&other, &["config", "--get", "core.hooksPath"]),
        "/elsewhere"
    );

    // heal works on a workspace `qq init` made: the session re-tag records the
    // identity, and a later heal restores tokens a herdr restart dropped.
    let live = FakePane::new(Some("sess-1"), &[]);
    let r = identity::session_retag(&live, Some("p1"), root, root).expect("eligible");
    assert_eq!(r.unwrap(), identity::Retag::Tagged);
    let rec: Value = serde_json::from_str(
        &std::fs::read_to_string(ws.runs_dir().join("cos.identity.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(rec, json!({"agent_session": "sess-1", "pane_id": "p1"}));
    let restarted = FakePane::new(Some("sess-1"), &[]);
    let done = identity::heal(&restarted, &ws.runs_dir()).unwrap();
    assert_eq!(done.len(), 1, "{done:?}");
    assert_eq!(restarted.token("grid_role").as_deref(), Some("cos"));
    // A pane without a session removes only its own record.
    let sessionless = FakePane::new(None, &[]);
    identity::record_identity(&sessionless, "other-pane", &ws.runs_dir()).unwrap();
    assert!(ws.runs_dir().join("cos.identity.json").is_file());
    identity::record_identity(&sessionless, "p1", &ws.runs_dir()).unwrap();
    assert!(!ws.runs_dir().join("cos.identity.json").exists());
}

#[test]
fn a_worker_worktree_pane_stays_untagged() {
    let ws = TestWorkspace::new("worker-wt");
    let root = ws.root();
    init::run(root, false, false).unwrap();
    let wt = root.join(".wt/w1");
    write(&wt.join("projects/INDEX.md"), "idx\n");
    let pane = FakePane::new(Some("s"), &[]);
    assert!(identity::session_retag(&pane, Some("p"), &wt, root).is_none());
    // A linked worktree marks itself with a `.git` file.
    let linked = root.join("linked");
    write(&linked.join(".git"), "gitdir: /x\n");
    write(&linked.join("projects/INDEX.md"), "idx\n");
    assert!(identity::session_retag(&pane, Some("p"), &linked, root).is_none());
    assert_eq!(*pane.writes.borrow(), 0);
    assert!(!ws.runs_dir().join("cos.identity.json").exists());
}

#[test]
fn registered_cmds_report_exit_codes() {
    let ws = TestWorkspace::new("cmd-codes");
    let env = Env {
        runs_dir: ws.runs_dir(),
    };
    let code = |r: Result<String, qq_core::cli::CliError>| r.map(|_| 0).unwrap_or_else(|e| e.code);
    assert_eq!(code(init::init_cmd(&env, &args(&["--bogus"]))), 2);
    assert_eq!(code(init::init_cmd(&env, &args(&["--git-hooks"]))), 2);
    assert_eq!(code(memory::memory_cmd(&env, &args(&["hook"]))), 2);
    assert_eq!(code(memory::inbox_cmd(&env, &args(&["--nope"]))), 2);
    assert_eq!(code(identity::identity_cmd(&env, &args(&["x"]))), 2);
    assert_eq!(code(transcripts::transcripts_cmd(&env, &args(&[]))), 2);
    assert_eq!(code(playbook::playbook_cmd(&env, &args(&["nope"]))), 2);

    // init: a locally changed template file is a conflict -> exit 3, kept, pending copy.
    assert_eq!(code(init::init_cmd(&env, &[])), 0);
    write(&ws.root().join("models.md"), "mine\n");
    let lock = std::fs::read_to_string(ws.root().join(".quaqua/template.lock")).unwrap();
    assert!(lock.contains("models.md"));
    assert_eq!(code(init::init_cmd(&env, &args(&["--update"]))), 3);
    assert_eq!(
        std::fs::read_to_string(ws.root().join("models.md")).unwrap(),
        "mine\n"
    );
    assert!(ws.root().join(".quaqua/pending/models.md").is_file());

    // The hooks fail open: no stdin, no answer, exit 0.
    assert_eq!(
        code(memory::memory_cmd_with(
            &env,
            &args(&["hook", "pre"]),
            String::new
        )),
        0
    );
    assert_eq!(
        code(memory::memory_cmd_with(
            &env,
            &args(&["hook", "batch"]),
            String::new
        )),
        0
    );
}

/// A test workspace must be sealed off from the live herdr: commands that
/// retag or inspect panes run against whatever `HERDR_*` the test process
/// inherited (a CoS pane's, when the suite runs from one). A recording herdr
/// stands in for the live one; nothing may call it.
#[test]
fn workspace_commands_never_reach_the_inherited_herdr() {
    let dir =
        Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("live-herdr-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let calls = dir.join("calls");
    let fake = dir.join("herdr");
    write(
        &fake,
        &format!("#!/bin/sh\necho \"$@\" >> {}\nexit 1\n", calls.display()),
    );
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    // What a live pane exports; set before the workspace takes the env lock.
    std::env::set_var("HERDR_BIN_PATH", &fake);
    std::env::set_var("HERDR_PANE_ID", "w9:p9");
    let ws = TestWorkspace::new("live-herdr");
    let env = Env {
        runs_dir: ws.runs_dir(),
    };
    let _ = init::init_cmd(&env, &[]);
    let _ = identity::identity_cmd(&env, &args(&["heal"]));
    let _ = memory::session_start(
        ws.root(),
        "{}",
        identity::cli_panes().as_ref(),
        identity::pane_from_env().as_deref(),
    );
    let seen = std::fs::read_to_string(&calls).unwrap_or_default();
    drop(ws);
    std::env::remove_var("HERDR_BIN_PATH");
    std::env::remove_var("HERDR_PANE_ID");
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(seen, "", "the live herdr was called");
}

#[test]
fn update_merges_memory_hooks_into_a_modified_settings() {
    let ws = TestWorkspace::new("settings-merge");
    let root = ws.root();
    init::run(root, false, false).unwrap();
    let settings = root.join(".claude/settings.json");
    write(
        &settings,
        r#"{"permissions":{"allow":["x"]},"hooks":{"Stop":[]}}"#,
    );
    let (report, c) = init::run(root, true, false).unwrap();
    assert!(
        c >= 1 && report.contains("conflict .claude/settings.json"),
        "{report}"
    );
    let doc: Value = serde_json::from_str(&std::fs::read_to_string(&settings).unwrap()).unwrap();
    assert_eq!(doc["permissions"]["allow"][0], "x", "user keys stay");
    let text = doc.to_string();
    assert!(
        text.contains("memory hook pre") && text.contains("memory hook batch"),
        "{text}"
    );
    assert!(text.contains("# qq-managed"));
    // Idempotent: a second update changes nothing.
    let before = std::fs::read_to_string(&settings).unwrap();
    init::run(root, true, false).unwrap();
    assert_eq!(std::fs::read_to_string(&settings).unwrap(), before);
}

#[test]
fn inbox_for_does_not_move_another_cursor() {
    let ws = TestWorkspace::new("inbox-guard");
    let env = Env {
        runs_dir: ws.runs_dir(),
    };
    let daemon = start(&ws);
    send(&ws, "peer-a", "do the thing");
    let inbox = |caller: &str, a: &[&str]| {
        std::env::set_var("QQ_AGENT", caller);
        let out = memory::inbox_cmd(&env, &args(a));
        std::env::remove_var("QQ_AGENT");
        out.unwrap()
    };
    // `lead` is neither peer-a nor the CoS nor peer-a's spawner: reads, never acks.
    assert!(inbox("lead", &["--for", "peer-a"]).contains("do the thing"));
    assert!(inbox("peer-a", &["--for", "peer-a", "--peek"]).contains("do the thing"));
    // The recipient itself acks.
    assert!(inbox("peer-a", &["--for", "peer-a"]).contains("do the thing"));
    assert_eq!(inbox("peer-a", &["--for", "peer-a"]), "");
    // A hook format fails open when the daemon cannot answer.
    daemon.stop().expect("daemon stops");
    assert_eq!(inbox("peer-a", &["--format", "hook-ups"]), "");
}
