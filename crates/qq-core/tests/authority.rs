//! T42: topology edges, the fence and the gate verb, observed through the
//! decision API, the refusal journal and the CLI exit status.

use std::sync::Arc;

use qq_core::authority::{
    self, check_mail, check_orchestration, check_prompt, check_question, check_spawn, Decision,
    Fence, Op, Role,
};
use qq_core::cli::{Env, Registry};
use qq_core::config::{self, Driver};
use qq_core::daemon::{Config, Daemon};
use qq_core::db;
use qq_core::herdr::FakeHerdr;
use qq_core::testkit::TestWorkspace;
use rusqlite::{params, Connection};

fn template() -> String {
    let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../templates/workspace/team.toml");
    std::fs::read_to_string(p)
        .expect("read template team.toml")
        .replace("harness = \"omp\"", "harness = \"claude\"")
}

/// A database seeded from the shipped team.toml, with projects 1 and 2.
fn setup(ws: &TestWorkspace) -> Connection {
    let mut conn = db::open(&ws.db_path()).expect("open qq db");
    config::seed(&mut conn, &template()).expect("seed template");
    for slug in ["alpha", "beta"] {
        conn.execute(
            "INSERT INTO project (slug, created_ms) VALUES (?1, 0)",
            [slug],
        )
        .unwrap();
    }
    conn
}

fn agent(
    conn: &Connection,
    name: &str,
    role: &str,
    project: Option<i64>,
    spawner: Option<i64>,
) -> i64 {
    conn.execute(
        "INSERT INTO agent (name, role, project_id, spawner_id, started_ms) VALUES (?1, ?2, ?3, ?4, 1)",
        params![name, role, project, spawner],
    )
    .unwrap();
    conn.last_insert_rowid()
}

fn edge(conn: &Connection, from: &str, to: &str, kind: &str) {
    conn.execute(
        "INSERT OR IGNORE INTO topology_edge (from_role, to_role, edge) VALUES (?1, ?2, ?3)",
        params![from, to, kind],
    )
    .unwrap();
}

fn refused(d: Decision) -> authority::Refusal {
    match d {
        Decision::Refused(r) => r,
        Decision::Allowed => panic!("expected a refusal"),
    }
}

fn allowed(d: Decision) {
    assert_eq!(d, Decision::Allowed);
}

#[test]
fn topology_refuses_off_edge() {
    let ws = TestWorkspace::new("auth-edges");
    let conn = setup(&ws);
    let cos = agent(&conn, "cos", "cos", None, None);
    let lead = agent(&conn, "lead-a", "lead", Some(1), Some(cos));
    agent(&conn, "lead-b", "lead", Some(2), Some(cos));
    agent(&conn, "p1", "peer", Some(1), Some(lead));

    // On the template's edges.
    allowed(check_spawn(&conn, "cos", Role::Peer, Some(1)).unwrap());
    allowed(check_spawn(&conn, "lead-a", Role::Peer, Some(1)).unwrap());
    allowed(check_mail(&conn, "lead-a", "cos").unwrap());
    allowed(check_mail(&conn, "cos", "lead-a").unwrap());
    allowed(check_prompt(&conn, "cos", "lead-a").unwrap());
    allowed(check_prompt(&conn, "lead-a", "p1").unwrap());
    assert!(authority::refusals(&conn).unwrap().is_empty());

    // Off the edges: each is refused and journaled with caller and edge.
    let r = refused(check_spawn(&conn, "lead-a", Role::Cos, None).unwrap());
    assert_eq!((r.caller.as_str(), r.edge.as_str()), ("lead-a", "spawn"));
    let r = refused(check_mail(&conn, "lead-a", "lead-b").unwrap());
    assert_eq!((r.caller.as_str(), r.edge.as_str()), ("lead-a", "mail"));
    let r = refused(check_prompt(&conn, "p1", "lead-a").unwrap());
    assert_eq!((r.caller.as_str(), r.edge.as_str()), ("p1", "prompt"));
    let r = refused(check_spawn(&conn, "ghost", Role::Peer, Some(1)).unwrap());
    assert_eq!(r.edge, "spawn");

    let log = authority::refusals(&conn).unwrap();
    let got: Vec<(&str, &str)> = log
        .iter()
        .map(|r| (r.caller.as_str(), r.edge.as_str()))
        .collect();
    assert_eq!(
        got,
        [
            ("lead-a", "spawn"),
            ("lead-a", "mail"),
            ("p1", "prompt"),
            ("ghost", "spawn")
        ]
    );
    // Same rows are plain `events` rows a reader of the journal can see.
    let n: i64 = conn
        .query_row(
            "SELECT count(*) FROM events WHERE workflow_id = 'authority:refusals' AND kind = 'refusal'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(n, 4);
}

#[test]
fn peer_to_peer_mail_refused() {
    let ws = TestWorkspace::new("auth-p2p");
    let conn = setup(&ws);
    let cos = agent(&conn, "cos", "cos", None, None);
    let lead = agent(&conn, "lead-a", "lead", Some(1), Some(cos));
    let other = agent(&conn, "lead-b", "lead", Some(2), Some(cos));
    let _ = other;
    agent(&conn, "p1", "peer", Some(1), Some(lead));
    agent(&conn, "p2", "peer", Some(1), Some(lead));

    // Refused even when the config grants a peer->peer mail edge.
    edge(&conn, "peer", "peer", "mail");
    let r = refused(check_mail(&conn, "p1", "p2").unwrap());
    assert_eq!((r.caller.as_str(), r.edge.as_str()), ("p1", "mail"));

    // A Peer mails its spawner, and only its spawner.
    allowed(check_mail(&conn, "p1", "lead-a").unwrap());
    refused(check_mail(&conn, "p1", "lead-b").unwrap());
    refused(check_mail(&conn, "p1", "cos").unwrap());
    assert_eq!(authority::refusals(&conn).unwrap().len(), 3);
}

#[test]
fn one_lead_per_project() {
    let ws = TestWorkspace::new("auth-lead");
    let conn = setup(&ws);
    agent(&conn, "cos", "cos", None, None);

    allowed(check_spawn(&conn, "cos", Role::Lead, Some(1)).unwrap());
    let lead = agent(&conn, "lead-a", "lead", Some(1), None);

    let r = refused(check_spawn(&conn, "cos", Role::Lead, Some(1)).unwrap());
    assert_eq!((r.caller.as_str(), r.edge.as_str()), ("cos", "spawn"));
    // Another project is free; a Lead that ended frees its own.
    allowed(check_spawn(&conn, "cos", Role::Lead, Some(2)).unwrap());
    conn.execute("UPDATE agent SET ended_ms = 5 WHERE id = ?1", [lead])
        .unwrap();
    allowed(check_spawn(&conn, "cos", Role::Lead, Some(1)).unwrap());
    assert_eq!(authority::refusals(&conn).unwrap().len(), 1);
}

#[test]
fn supervisor_question_edge_only() {
    let ws = TestWorkspace::new("auth-sup");
    let conn = setup(&ws);
    // The Supervisor's one edge: a question to Leads.
    edge(&conn, "supervisor", "lead", "question");
    let cos = agent(&conn, "cos", "cos", None, None);
    let lead = agent(&conn, "lead-a", "lead", Some(1), Some(cos));
    agent(&conn, "p1", "peer", Some(1), Some(lead));
    agent(&conn, "sup", "supervisor", None, None);

    allowed(check_question(&conn, "sup", "lead-a").unwrap());
    assert!(authority::refusals(&conn).unwrap().is_empty());

    // Nothing else reaches a Lead, and nothing reaches a Peer.
    refused(check_mail(&conn, "sup", "lead-a").unwrap());
    refused(check_prompt(&conn, "sup", "lead-a").unwrap());
    refused(check_spawn(&conn, "sup", Role::Lead, Some(2)).unwrap());
    refused(check_question(&conn, "sup", "p1").unwrap());
    refused(check_mail(&conn, "sup", "p1").unwrap());
    refused(check_question(&conn, "lead-a", "sup").unwrap());
    let log = authority::refusals(&conn).unwrap();
    assert_eq!(log.len(), 6);
    assert!(log
        .iter()
        .all(|r| !r.caller.is_empty() && !r.edge.is_empty()));
}

#[test]
fn unfenced_driver_cannot_push() {
    let ws = TestWorkspace::new("auth-fence");
    let conn = setup(&ws);
    agent(&conn, "lead-a", "lead", Some(1), None);

    assert_eq!(authority::fence_for(Driver::Generic), Fence::Unfenced);
    assert_eq!(authority::fence_for(Driver::Native), Fence::Fenced);

    allowed(check_orchestration(&conn, "lead-a", Op::Push, Fence::Fenced).unwrap());
    let r = refused(check_orchestration(&conn, "lead-a", Op::Push, Fence::Unfenced).unwrap());
    assert_eq!((r.caller.as_str(), r.edge.as_str()), ("lead-a", "push"));
    // An unfenced driver still builds and merges in its worktree.
    allowed(check_orchestration(&conn, "lead-a", Op::Merge, Fence::Unfenced).unwrap());
    assert_eq!(authority::refusals(&conn).unwrap().len(), 1);
}

#[test]
fn peer_identity_orchestration_refused() {
    let ws = TestWorkspace::new("auth-peer");
    let conn = setup(&ws);
    let lead = agent(&conn, "lead-a", "lead", Some(1), None);
    agent(&conn, "p1", "peer", Some(1), Some(lead));

    for op in [Op::Spawn, Op::Merge, Op::Push, Op::ConfigWrite] {
        for fence in [Fence::Fenced, Fence::Unfenced] {
            let r = refused(check_orchestration(&conn, "p1", op, fence).unwrap());
            assert_eq!(
                (r.caller.as_str(), r.edge.as_str()),
                ("p1", "orchestration")
            );
        }
        allowed(check_orchestration(&conn, "lead-a", op, Fence::Fenced).unwrap());
    }
    // A Peer cannot spawn through the topology door either.
    refused(check_spawn(&conn, "p1", Role::Peer, Some(1)).unwrap());
    assert_eq!(authority::refusals(&conn).unwrap().len(), 9);
}

#[test]
fn gate_verb_blocks_pane_and_push() {
    let ws = TestWorkspace::new("auth-gate");
    let mut registry = Registry::new();
    let mut daemon = Daemon::new(Config {
        runs_dir: ws.runs_dir(),
        herdr: Arc::new(FakeHerdr::new()),
    });
    authority::INSTALL(&mut registry, &mut daemon).expect("install");
    assert!(
        authority::INSTALL(&mut registry, &mut daemon).is_err(),
        "install twice is refused"
    );
    let env = Env {
        runs_dir: ws.runs_dir(),
    };

    let hook = |cmd: &str| {
        let json = serde_json::json!({"tool_name": "Bash", "tool_input": {"command": cmd}});
        registry.dispatch(&env, &["gate".into(), "--json".into(), json.to_string()])
    };

    for cmd in [
        "herdr pane split --direction right",
        "herdr workspace create --label x",
        "herdr agent start --kind claude",
        "cd /tmp && git push origin HEAD",
        "git -C /repo push --force",
        "bash -c 'git push origin main'",
        "sudo -u me env FOO=1 herdr tab create",
        "echo $(herdr pane move x)",
    ] {
        assert_eq!(hook(cmd), 2, "must block: {cmd}");
    }
    for cmd in [
        "git status",
        "git push --help",
        "git log --oneline",
        "herdr pane list",
        "herdr pane split --help",
        "echo 'git push' | wc -c",
        "cargo test --locked -p qq-core authority",
    ] {
        assert_eq!(hook(cmd), 0, "must allow: {cmd}");
    }
    // Input the gate cannot parse fails closed, empty input included.
    assert_eq!(
        registry.dispatch(&env, &["gate".into(), "--json".into(), "{not json".into()]),
        2
    );
    assert_eq!(
        registry.dispatch(&env, &["gate".into(), "--json".into(), String::new()]),
        2
    );
}

fn gate_exit(cmd: &str) -> i32 {
    let ws = TestWorkspace::new("auth-gate-x");
    let mut registry = Registry::new();
    let mut daemon = Daemon::new(Config {
        runs_dir: ws.runs_dir(),
        herdr: Arc::new(FakeHerdr::new()),
    });
    authority::INSTALL(&mut registry, &mut daemon).unwrap();
    let env = Env {
        runs_dir: ws.runs_dir(),
    };
    let json = serde_json::json!({"tool_name": "Bash", "tool_input": {"command": cmd}});
    registry.dispatch(&env, &["gate".into(), "--json".into(), json.to_string()])
}

#[test]
fn gate_wrappers_cannot_hide_push() {
    for cmd in [
        "timeout 60 git push origin x",
        "timeout -s KILL 5 git push",
        "xargs -I{} git push",
        "xargs -I {} git push origin {}",
        "xargs -n1 git push",
        "xargs -P 4 -n 1 git push",
        "echo x | xargs git push",
        "env -S 'git push origin x'",
        "env --split-string='git push'",
        "find . -exec git push \\;",
        "find . -execdir git push {} +",
        "echo 'git push' | bash",
        "echo 'git push' | sh",
        "echo 'git push' | zsh -s",
        "bash -s <<< 'git push'",
        "sh <<EOF\ngit push\nEOF",
        "ksh -c 'git push'",
        "fish -c 'git push'",
        "dash -c 'git push'",
        "busybox sh -c 'git push'",
        "git -c alias.p=push p",
        "git -calias.p='push origin' p",
        "git -c alias.p='!git push' p",
    ] {
        assert_eq!(gate_exit(cmd), 2, "must block: {cmd}");
    }
}

#[test]
fn gate_wrapper_negatives() {
    for cmd in [
        "timeout 60 cargo test",
        "xargs -I{} echo {}",
        "xargs -n1 git status",
        "env -S 'git status'",
        "env FOO=1 git log",
        "find . -name '*.rs' -exec grep push {} +",
        "find . -exec git status {} \\;",
        "echo 'git push' | wc -c",
        "echo hello | bash",
        "bash scripts/build.sh",
        "bash -s <<< 'echo hi'",
        "ksh -c 'echo hi'",
        "busybox sh -c 'ls'",
        "git -c user.name=x status",
        "git -c alias.st=status st",
        "gh pr view 12",
        "herdr pane list",
    ] {
        assert_eq!(gate_exit(cmd), 0, "must allow: {cmd}");
    }
}

#[test]
fn gate_blocks_gh_merge() {
    for cmd in [
        "gh pr merge 12",
        "gh pr merge --squash --auto",
        "gh pr -R o/r merge 12",
        "gh api -X PUT repos/o/r/pulls/12/merge",
        "gh api --method POST repos/o/r/merges -f base=main -f head=x",
        "gh api repos/o/r/pulls/12/merge -X PUT",
        "gh api graphql -f query='mutation { mergePullRequest(input:{pullRequestId:\"x\"}) { clientMutationId } }'",
        "echo go && gh pr merge 3",
    ] {
        assert_eq!(gate_exit(cmd), 2, "must block: {cmd}");
    }
    for cmd in [
        "gh pr view 12",
        "gh pr list",
        "gh api repos/o/r/pulls/12",
        "gh api -X GET repos/o/r/pulls/12/merge",
    ] {
        assert_eq!(gate_exit(cmd), 0, "must allow: {cmd}");
    }
}

#[test]
fn gate_blocks_close() {
    for cmd in [
        "herdr pane close 1-2",
        "herdr pane close --current",
        "herdr tab close 1",
        "herdr workspace close w",
        "herdr --session x pane close p",
    ] {
        assert_eq!(gate_exit(cmd), 2, "must block: {cmd}");
    }
    assert_eq!(gate_exit("herdr pane close --help"), 0);
    assert_eq!(gate_exit("herdr pane read 1-2"), 0);
}

#[test]
fn gate_fails_closed() {
    for json in [
        "",
        "   ",
        "{}",
        r#"{"tool_input":{}}"#,
        r#"{"tool_input":{"command":5}}"#,
        r#"{"tool_input":{"command":null}}"#,
        r#"{"tool_input":{"command":""}}"#,
        "not json",
    ] {
        assert_eq!(
            authority::gate_evaluate(json),
            authority::GateVerdict::Block(
                "blocked: spawn/move panes and workspaces only through the qq daemon (qq call task.spawn / grid verbs)"
            ),
            "must block: {json:?}"
        );
    }
    struct Broken;
    impl std::io::Read for Broken {
        fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
            Err(std::io::Error::other("stdin gone"))
        }
    }
    assert!(matches!(
        authority::gate_evaluate_reader(Broken),
        authority::GateVerdict::Block(_)
    ));
    let ok = br#"{"tool_input":{"command":"ls"}}"#;
    assert_eq!(
        authority::gate_evaluate_reader(&ok[..]),
        authority::GateVerdict::Allow
    );
}

#[test]
fn supervisor_has_no_peer_edge_even_with_config() {
    let ws = TestWorkspace::new("auth-sup-peer");
    let conn = setup(&ws);
    // A config that grants every Supervisor -> Peer edge still gets refused.
    for kind in ["spawn", "mail", "report", "question"] {
        edge(&conn, "supervisor", "peer", kind);
    }
    conn.execute(
        "INSERT INTO setting (key, value_json) VALUES ('roles.supervisor.can_prompt', '[\"peer\"]')",
        [],
    )
    .unwrap();
    let cos = agent(&conn, "cos", "cos", None, None);
    let lead = agent(&conn, "lead-a", "lead", Some(1), Some(cos));
    agent(&conn, "p1", "peer", Some(1), Some(lead));
    agent(&conn, "sup", "supervisor", None, None);

    refused(check_spawn(&conn, "sup", Role::Peer, Some(1)).unwrap());
    refused(check_mail(&conn, "sup", "p1").unwrap());
    refused(check_prompt(&conn, "sup", "p1").unwrap());
    refused(check_question(&conn, "sup", "p1").unwrap());
    assert_eq!(authority::refusals(&conn).unwrap().len(), 4);
}

#[test]
fn lead_spawn_needs_project() {
    let ws = TestWorkspace::new("auth-lead-np");
    let conn = setup(&ws);
    agent(&conn, "cos", "cos", None, None);
    let r = refused(check_spawn(&conn, "cos", Role::Lead, None).unwrap());
    assert_eq!((r.caller.as_str(), r.edge.as_str()), ("cos", "spawn"));
    allowed(check_spawn(&conn, "cos", Role::Lead, Some(1)).unwrap());
    // A Peer may still be spawned without a project (a CoS-owned worker).
    allowed(check_spawn(&conn, "cos", Role::Peer, None).unwrap());
}

#[test]
fn lead_mail_scoped_to_own_project() {
    let ws = TestWorkspace::new("auth-lead-scope");
    let conn = setup(&ws);
    let cos = agent(&conn, "cos", "cos", None, None);
    let a = agent(&conn, "lead-a", "lead", Some(1), Some(cos));
    let b = agent(&conn, "lead-b", "lead", Some(2), Some(cos));
    agent(&conn, "pa", "peer", Some(1), Some(a));
    agent(&conn, "pb", "peer", Some(2), Some(b));
    // A Peer of project 1 spawned by someone else, to prove spawner also counts.
    agent(&conn, "px", "peer", Some(2), Some(a));

    allowed(check_mail(&conn, "lead-a", "pa").unwrap());
    allowed(check_mail(&conn, "lead-a", "px").unwrap());
    let r = refused(check_mail(&conn, "lead-a", "pb").unwrap());
    assert_eq!(
        (r.caller.as_str(), r.edge.as_str(), r.target.as_str()),
        ("lead-a", "mail", "pb")
    );
    allowed(check_mail(&conn, "lead-b", "pb").unwrap());
    refused(check_mail(&conn, "lead-b", "pa").unwrap());
}

#[test]
fn unreadable_database_is_an_error_not_an_allow() {
    // No schema at all: the decision cannot be made, so every check errs and
    // the caller must refuse.
    let conn = Connection::open_in_memory().unwrap();
    assert!(check_spawn(&conn, "cos", Role::Peer, Some(1)).is_err());
    assert!(check_mail(&conn, "a", "b").is_err());
    assert!(check_prompt(&conn, "a", "b").is_err());
    assert!(check_question(&conn, "a", "b").is_err());
    assert!(check_orchestration(&conn, "a", Op::Push, Fence::Fenced).is_err());
}

#[test]
fn unknown_caller_refusal_records_attempted_target() {
    let ws = TestWorkspace::new("auth-unknown");
    let conn = setup(&ws);
    agent(&conn, "lead-a", "lead", Some(1), None);
    refused(check_spawn(&conn, "ghost", Role::Peer, Some(1)).unwrap());
    refused(check_mail(&conn, "ghost", "lead-a").unwrap());
    refused(check_orchestration(&conn, "ghost", Op::Merge, Fence::Fenced).unwrap());
    let log = authority::refusals(&conn).unwrap();
    let got: Vec<(&str, &str, &str)> = log
        .iter()
        .map(|r| (r.caller.as_str(), r.edge.as_str(), r.target.as_str()))
        .collect();
    assert_eq!(
        got,
        [
            ("ghost", "spawn", "peer"),
            ("ghost", "mail", "lead-a"),
            ("ghost", "orchestration", "merge"),
        ]
    );
}
