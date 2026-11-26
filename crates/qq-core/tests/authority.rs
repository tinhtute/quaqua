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
    let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/team.toml");
    std::fs::read_to_string(p).expect("read template team.toml")
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
                "blocked: spawn/move panes and workspaces only through the quaqua daemon (quaqua call task.spawn / grid verbs)"
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

// U3 gate tripwires: (name, must block, must allow). The same table drives the
// binary verdict and the shell fallback, so the two stay in step.
type Tripwire = (
    &'static str,
    &'static [&'static str],
    &'static [&'static str],
);

const TRIPWIRES: &[Tripwire] = &[
    (
        "alias push",
        &[
            "git config alias.p push",
            "git config --global alias.p push",
            "git config --local alias.ship 'push origin main'",
            "git config alias.p '!git push'",
            "git -C repo config --add alias.p push",
            "git config alias.p \"!f() { git push origin main; }; f\"",
            "git config ALIAS.p push",
            "git -c ALIAS.p=push status",
        ],
        &[
            "git config alias.st status",
            "git config user.name x",
            "git config --get alias.p",
            "git config ALIAS.st status",
        ],
    ),
    (
        "gh api ref and contents writes",
        &[
            "gh api -X PATCH repos/o/r/git/refs/heads/main -f sha=abc",
            "gh api --method PATCH repos/o/r/git/refs/heads/main -f sha=abc",
            "gh api --method=PATCH repos/o/r/git/refs/heads/main -f sha=abc",
            "gh api -XPATCH repos/o/r/git/refs/heads/main",
            "gh api repos/o/r/git/refs/heads/main -X PATCH -f sha=abc",
            "gh api -X patch repos/o/r/git/refs/heads/main",
            "gh api -X PUT repos/o/r/contents/README.md -f message=x -f content=eA==",
            "gh api -X POST repos/o/r/git/refs -f ref=refs/heads/main -f sha=abc",
            "gh api -X DELETE repos/o/r/git/refs/heads/main",
            "gh api -X 'DELETE' repos/o/r/git/refs/heads/main",
            "gh api -X \"PATCH\" repos/o/r/git/refs/heads/main",
            "gh api -X=DELETE repos/o/r/git/refs/heads/main",
            "gh api --method='DELETE' repos/o/r/git/refs/heads/main",
            "gh api --method \"PUT\" repos/o/r/contents/README.md",
            "gh api -X GET -X DELETE repos/o/r/git/refs/heads/main",
            "gh api --method GET --method PUT repos/o/r/contents/a.txt -f message=m",
            "gh api --method=GET --method=PATCH repos/o/r/git/refs/heads/main",
            "gh api -X GET -XPATCH repos/o/r/git/refs/heads/main",
            "gh api -X DELETE repos/o/r/git/refs/heads/main -p -XGET",
            "gh api -X DELETE repos/o/r/git/refs/heads/main --jq -XGET",
            "gh api -X DELETE repos/o/r/git/refs/heads/main -H -XGET",
            "gh api -X DELETE -X GET repos/o/r/git/refs/heads/main",
            "gh api repos/o/r/git/refs -p -XGET -f sha=x",
            "gh api -p -XGET -f sha=x repos/o/r/git/refs",
            "gh api --jq -XGET -F ref=x repos/o/r/git/refs",
            "gh api -H -XGET repos/o/r/merges -f base=main -f head=x",
            "gh api -H -XGET repos/o/r/git/refs/heads/main --input body.json",
            "gh api repos/o/r/git/refs -f ref=refs/heads/x -f sha=abc",
        ],
        &[
            "gh api repos/o/r/git/refs/heads/main",
            "gh api -X GET repos/o/r/git/refs/heads/main",
            "gh api --method GET repos/o/r/contents/README.md",
            "gh api -X GET repos/o/r/git/refs/heads/main -p foo",
            "gh api repos/o/r/contents/README.md --jq .content",
            "gh api repos/o/r/contents/README.md -q .content -H 'Accept: x'",
            "gh api repos/o/r/contents/README.md",
            "gh api -X POST repos/o/r/issues/1/comments -f body=x",
        ],
    ),
    (
        "graphql enablePullRequestAutoMerge",
        &[
            "gh api graphql -f query='mutation { enablePullRequestAutoMerge(input:{pullRequestId:\"x\"}) { clientMutationId } }'",
            "gh api graphql -f query=\"mutation { enablePullRequestAutoMerge(input:{pullRequestId:\\\"x\\\"}) { clientMutationId } }\"",
            "gh api graphql -f query=\"mutation { mergePullRequest(input:{pullRequestId:\\\"x\\\"}) { clientMutationId } }\"",
            "gh api graphql -f query='mutation { mergePullRequest(input:{pullRequestId:\"x\"}) { clientMutationId } }'",
        ],
        &[
            "gh api graphql -f query='{ viewer { login } }'",
            "gh api graphql -f query=\"{ viewer { login } }\"",
        ],
    ),
    (
        "graphql query from a file",
        &[
            "gh api graphql -F query=@auto.graphql",
            "gh api graphql -Fquery=@auto.graphql",
            "gh api graphql --field query=@auto.graphql",
            "gh api graphql --field=query=@auto.graphql",
        ],
        &[],
    ),
    (
        "herdr pane input",
        &[
            "herdr pane send-keys w1:p1 enter",
            "herdr pane send-text w1:p1 'hello'",
            "herdr pane run w1:p1 'rm -rf x'",
            "herdr --session x pane send-keys p enter",
            "echo hi && herdr pane run p ls",
        ],
        &[
            "herdr agent send-keys worker enter",
            "herdr agent prompt worker hi",
            "quaqua worker prompt --resume worker",
            "herdr pane read p",
            "herdr pane list",
        ],
    ),
    (
        "grid_role metadata",
        &[
            "herdr pane report-metadata p --source s --token grid_role=cos",
            "herdr pane report-metadata p --source s --token=grid_role=cos",
            "herdr pane report-metadata p --token grid_role=worker --source s",
        ],
        &["herdr pane report-metadata p --source s --token role=x --display-agent Foo"],
    ),
    (
        "sqlite3 on qq.db",
        &[
            "sqlite3 runs/qq.db 'delete from tasks'",
            "sqlite3 ~/quaqua/runs/qq.db \"update agents set kind='cos'\"",
            "sqlite3 runs/qq.db < fix.sql",
            "cat fix.sql | sqlite3 runs/qq.db",
            "sqlite3 -cmd '.timeout 5' runs/qq.db 'insert into t values (1)'",
            "sqlite3 -readonly :memory: \".open runs/qq.db\" \"delete from tasks\"",
            "sqlite3 -readonly :memory: \"ATTACH 'runs/qq.db' AS q\" \"delete from q.tasks\"",
            "sqlite3 --readonly :memory: \"attach 'runs/qq.db' as q\" \"delete from q.tasks\"",
        ],
        &[
            "sqlite3 -readonly runs/qq.db 'select 1'",
            "sqlite3 --readonly runs/qq.db 'select 1'",
            "sqlite3 -readonly ~/quaqua/runs/qq.db < q.sql",
            "sqlite3 other.db 'select 1'",
            "sqlite3 -readonly runs/qq.db 'select attached_at from t'",
        ],
    ),
    (
        "delete main or master",
        &[
            "git branch -D main",
            "git branch -D master",
            "git branch -d main",
            "git branch --delete --force main",
            "git branch -d -f master",
            "git branch -df main",
            "git -C repo branch -D main",
            "git branch -D feat main",
            "git branch -D refs/heads/main",
            "git branch main -D",
            "git branch master -d",
            "git branch main --delete",
            "git branch refs/heads/main -D",
        ],
        &[
            "git branch --list main",
            "git branch main",
            "git branch -a",
            "git branch main && ls -d x",
        ],
    ),
];

fn tripwire(name: &str) -> &'static Tripwire {
    TRIPWIRES.iter().find(|t| t.0 == name).unwrap()
}

/// One test per behaviour: the binary verdict, then the shell fallback.
fn assert_tripwire(name: &str) {
    let (_, blocked, allowed) = tripwire(name);
    for cmd in *blocked {
        assert_eq!(gate_exit(cmd), 2, "{name}: must block: {cmd}");
        assert_eq!(fallback_exit(cmd), 2, "{name}: fallback must block: {cmd}");
    }
    for cmd in *allowed {
        assert_eq!(gate_exit(cmd), 0, "{name}: must allow: {cmd}");
        assert_eq!(fallback_exit(cmd), 0, "{name}: fallback must allow: {cmd}");
    }
}

#[test]
fn gate_blocks_alias_push() {
    assert_tripwire("alias push");
}

#[test]
fn gate_blocks_gh_api_ref_and_contents_writes() {
    assert_tripwire("gh api ref and contents writes");
}

#[test]
fn gate_blocks_graphql_auto_merge() {
    assert_tripwire("graphql enablePullRequestAutoMerge");
}

#[test]
fn gate_blocks_graphql_query_from_file() {
    assert_tripwire("graphql query from a file");
}

#[test]
fn gate_blocks_herdr_pane_input() {
    assert_tripwire("herdr pane input");
    // The fallback is a superset and cannot tell `--help`; the binary can.
    assert_eq!(gate_exit("herdr pane send-keys --help"), 0);
}

#[test]
fn gate_any_non_get_method_blocks() {
    // gh keeps the last -X/--method, but `-p -XGET` smuggles a GET token after a
    // DELETE: any explicit non-GET method blocks, whatever the order.
    assert_eq!(
        gate_exit("gh api -X DELETE -X GET repos/o/r/git/refs/heads/main"),
        2
    );
    assert_eq!(gate_exit("gh api -X GET -X PUT repos/o/r/pulls/1/merge"), 2);
    assert_eq!(gate_exit("gh api -X PUT -X GET repos/o/r/pulls/1/merge"), 2);
    // Every explicit method GET stays a read.
    assert_eq!(gate_exit("gh api -X GET -XGET repos/o/r/pulls/1/merge"), 0);
}

#[test]
fn gate_blocks_grid_role_metadata() {
    assert_tripwire("grid_role metadata");
}

#[test]
fn gate_blocks_qq_db_writes() {
    assert_tripwire("sqlite3 on qq.db");
}

#[test]
fn gate_blocks_main_or_master_delete() {
    assert_tripwire("delete main or master");
}

fn fallback_exit(cmd: &str) -> i32 {
    use std::io::Write;
    let script = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("templates/workspace/scripts/hooks/herdr-gate.sh");
    let json = serde_json::json!({"tool_name": "Bash", "tool_input": {"command": cmd}});
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

// ---- caller-aware tier gate (R5, R7, R14) -------------------------------

mod caller_gate {
    use super::*;
    use qq_core::authority::tier::{self, Action, Decision, Tier};
    use qq_core::authority::{gate_evaluate_in, GateVerdict};
    use qq_core::daemon::DaemonHandle;
    use qq_core::herdr::{HerdrClient, Verb};
    use qq_core::identity::peer::HerdrPeers;
    use qq_core::rpc::{self, Request};
    use serde_json::{json, Value};

    #[derive(Clone, Copy, PartialEq)]
    pub enum Who {
        Cos,
        Worker,
        Lead,
        Unmapped,
        Down,
    }

    pub struct Rig {
        pub ws: TestWorkspace,
        herdr: Arc<FakeHerdr>,
        daemon: Option<DaemonHandle>,
    }

    fn me() -> u32 {
        std::process::id()
    }

    pub fn rig(tag: &str, who: Who) -> Rig {
        let ws = TestWorkspace::new(tag);
        ws.seal_herdr();
        let herdr = Arc::new(FakeHerdr::new());
        if who == Who::Down {
            return Rig {
                ws,
                herdr,
                daemon: None,
            };
        }
        let mut d = Daemon::new(Config {
            runs_dir: ws.runs_dir(),
            herdr: herdr.clone() as Arc<dyn HerdrClient>,
        });
        d.require_peer(Arc::new(HerdrPeers::new(herdr.clone())));
        let daemon = d.spawn().unwrap();
        match who {
            Who::Cos => {
                let p = herdr.seed_pane("c", Some("claude"), &[("grid_role", "cos")]);
                herdr.set_pane_pids(&p, me(), &[]);
            }
            Who::Worker | Who::Lead | Who::Unmapped => {
                let p = herdr.seed_pane("w", None, &[]);
                let name = if who == Who::Lead { "lead-1" } else { "w-1" };
                herdr.agent_start(name, "claude", &p, 0, &[]).unwrap();
                herdr.set_pane_pids(&p, if who == Who::Unmapped { 1 } else { me() }, &[]);
            }
            Who::Down => unreachable!(),
        }
        Rig {
            ws,
            herdr,
            daemon: Some(daemon),
        }
    }

    impl Drop for Rig {
        fn drop(&mut self) {
            if let Some(d) = self.daemon.take() {
                self.herdr.clear(Verb::PaneList);
                let p = self.herdr.seed_pane("stop", None, &[]);
                self.herdr
                    .agent_start("stopper", "claude", &p, 0, &[])
                    .unwrap();
                self.herdr.set_pane_pids(&p, me(), &[]);
                let _ = d.stop();
            }
        }
    }

    impl Rig {
        fn env(&self) -> Env {
            Env {
                runs_dir: self.ws.runs_dir(),
            }
        }
        pub fn bash(&self, cmd: &str) -> GateVerdict {
            self.run(json!({"tool_name": "Bash", "tool_input": {"command": cmd}}))
        }
        pub fn edit(&self, tool: &str, path: &str) -> GateVerdict {
            self.run(json!({"tool_name": tool, "tool_input": {"file_path": path}}))
        }
        pub fn run(&self, hook: Value) -> GateVerdict {
            gate_evaluate_in(&self.env(), &hook.to_string())
        }
        pub fn lookup(&self) -> Result<Value, rpc::RpcError> {
            let sock = rpc::socket_path(&self.ws.runs_dir());
            rpc::call(&sock, &Request::new("caller.lookup", Value::Null))
                .map_err(|e| rpc::RpcError::new("io", e.to_string()))?
                .into_result()
        }
    }

    pub fn is_block(v: &GateVerdict) -> bool {
        matches!(v, GateVerdict::Block(_))
    }
    pub fn is_ask(v: &GateVerdict) -> bool {
        matches!(v, GateVerdict::Ask(_))
    }

    /// cos asks, worker/Lead/unmapped/daemon-down block.
    pub fn assert_ask_for_cos_deny_for_rest(tag: &str, cmd: &str) {
        {
            let r = rig(&format!("{tag}-cos"), Who::Cos);
            assert!(is_ask(&r.bash(cmd)), "{tag}: cos must ask: {cmd}");
        }
        assert_denied_for_rest(tag, cmd);
    }

    pub fn assert_denied_for_rest(tag: &str, cmd: &str) {
        for (who, n) in [
            (Who::Worker, "w"),
            (Who::Lead, "l"),
            (Who::Unmapped, "u"),
            (Who::Down, "d"),
        ] {
            let r = rig(&format!("{tag}-{n}"), who);
            assert!(is_block(&r.bash(cmd)), "{tag}: non-cos must deny: {cmd}");
        }
    }

    // ---- caller_lookup_* ----

    #[test]
    fn caller_lookup_cos_passes() {
        let r = rig("cl-cos", Who::Cos);
        let v = r.lookup().unwrap();
        assert_eq!(v["caller"], "cos");
        assert_eq!(v["cos"], true);
    }

    #[test]
    fn caller_lookup_worker_is_not_cos() {
        {
            let r = rig("cl-worker", Who::Worker);
            let v = r.lookup().unwrap();
            assert_eq!(v["caller"], "w-1");
            assert_eq!(v["cos"], false);
        }
        let r = rig("cl-lead", Who::Lead);
        assert_eq!(r.lookup().unwrap()["cos"], false);
    }

    #[test]
    fn caller_lookup_unmapped_peer_is_refused() {
        let r = rig("cl-unmapped", Who::Unmapped);
        assert_eq!(r.lookup().unwrap_err().code, "unmapped_peer");
        assert!(is_block(&r.bash("git tag v1")));
    }

    #[test]
    fn caller_lookup_daemon_down_denies_listed_hit_only() {
        let r = rig("cl-down", Who::Down);
        assert!(is_block(&r.bash("git tag v1")));
        assert!(is_block(&r.bash("gh release create v1")));
        // Never asked on an ordinary call: no daemon, still allowed.
        for ok in ["git status", "ls -la", "git branch --list", "cargo test"] {
            assert_eq!(r.bash(ok), GateVerdict::Allow, "{ok}");
        }
        assert_eq!(r.edit("Write", "/tmp/notes.md"), GateVerdict::Allow);
    }

    #[test]
    fn caller_lookup_without_a_peer_resolver_is_refused() {
        // A daemon that does not name callers must not let a claim stand.
        let ws = TestWorkspace::new("cl-nopeer");
        ws.seal_herdr();
        let d = Daemon::new(Config {
            runs_dir: ws.runs_dir(),
            herdr: Arc::new(FakeHerdr::new()),
        })
        .spawn()
        .unwrap();
        let mut req = Request::new("caller.lookup", Value::Null);
        req.caller = Some("cos".into());
        let sock = rpc::socket_path(&ws.runs_dir());
        let res = rpc::call(&sock, &req).unwrap().into_result();
        assert!(res.is_err(), "{res:?}");
        let _ = d.stop();
    }

    // ---- tier_table_* ----

    const A: Decision = Decision::Allow;
    const K: Decision = Decision::Ask;
    const D: Decision = Decision::Deny;

    /// One cos case per (tier, action): repo tier low/medium/high, lane low.
    fn row(action: Action, want: [Decision; 3]) {
        for (i, t) in [Tier::Low, Tier::Medium, Tier::High]
            .into_iter()
            .enumerate()
        {
            assert_eq!(
                tier::decide(true, action, t, Tier::Low),
                want[i],
                "cos {action:?} at repo tier {t:?}"
            );
            assert_eq!(
                tier::decide(false, action, t, Tier::Low),
                D,
                "non-cos {action:?} at {t:?}"
            );
        }
    }

    #[test]
    fn tier_table_push_non_main() {
        row(Action::PushNonMain, [A, A, A]);
    }
    #[test]
    fn tier_table_open_pr() {
        row(Action::OpenPr, [A, A, A]);
    }
    #[test]
    fn tier_table_delete_merged_branch() {
        row(Action::BranchDeleteMerged, [A, A, K]);
    }
    #[test]
    fn tier_table_delete_unmerged_branch() {
        row(Action::BranchDeleteUnmerged, [K, K, K]);
    }
    #[test]
    fn tier_table_merge_request() {
        row(Action::MergeRequest, [A, A, K]);
    }
    #[test]
    fn tier_table_tag_release_delete_data() {
        row(Action::Tag, [A, A, K]);
        row(Action::Release, [A, A, K]);
        row(Action::DeleteData, [A, A, K]);
    }
    #[test]
    fn tier_table_history_rewrite() {
        row(Action::HistoryRewrite, [K, K, K]);
    }
    #[test]
    fn tier_table_floor_asks_at_every_tier() {
        row(Action::Floor, [K, K, K]);
    }
    #[test]
    fn tier_table_main_push_and_force_push_deny_always() {
        row(Action::PushMain, [D, D, D]);
        row(Action::ForcePush, [D, D, D]);
    }
    #[test]
    fn tier_table_guard_actions() {
        row(Action::TrunkMove, [K, K, K]);
        row(Action::Bypass, [K, K, K]);
        row(Action::DbWrite, [K, K, K]);
        row(Action::InstructionFile, [K, K, K]);
        for t in [Tier::Low, Tier::Medium, Tier::High] {
            assert_eq!(tier::decide(true, Action::PaneInput, t, Tier::Low), A);
            assert_eq!(tier::decide(false, Action::PaneInput, t, Tier::Low), D);
        }
    }
    #[test]
    fn tier_table_strictest_of_repo_lane_and_action_wins() {
        // Low repo, low action, high lane: the lane decides.
        assert_eq!(tier::decide(true, Action::Tag, Tier::Low, Tier::High), K);
        assert_eq!(
            tier::decide(true, Action::PushNonMain, Tier::Low, Tier::High),
            A
        );
        assert_eq!(
            tier::decide(true, Action::Tag, Tier::Medium, Tier::Medium),
            A
        );
        assert_eq!(
            tier::action_tier(Action::BranchDeleteMerged),
            Some(Tier::Low)
        );
        assert_eq!(tier::action_tier(Action::Tag), Some(Tier::Medium));
        assert_eq!(tier::action_tier(Action::HistoryRewrite), Some(Tier::High));
    }

    // ---- ask_tier_* ----

    #[test]
    fn ask_tier_unlisted_repo_is_high() {
        // The stub reader: no daemon row, so high.
        let ws = TestWorkspace::new("ask-repo");
        assert_eq!(tier::repo_tier(&ws.runs_dir(), ws.root()), Tier::High);
        assert_eq!(tier::lane_tier(), Tier::High);
    }

    #[test]
    fn ask_tier_tag_and_release() {
        assert_ask_for_cos_deny_for_rest("ask-tag", "git tag v1.2.3");
        assert_ask_for_cos_deny_for_rest("ask-tag-a", "git tag -a v1 -m x");
        assert_ask_for_cos_deny_for_rest("ask-tag-d", "git tag -d v1");
        assert_ask_for_cos_deny_for_rest("ask-rel", "gh release create v1 --notes x");
    }

    /// The one canonical merge line asks the CoS and blocks everyone else.
    #[test]
    fn ask_tier_pr_merge_canonical_asks_for_cos() {
        for (tag, cmd) in [
            ("ask-merge", "gh pr merge 5"),
            ("ask-merge-zeros", "gh pr merge 007"),
            ("ask-merge-repo", "gh pr merge 5 --repo o/r"),
            ("ask-merge-r", "gh pr merge 5 -R o/r --squash"),
            ("ask-merge-rebase", "gh pr merge 5 --rebase --delete-branch"),
            ("ask-merge-sha", "gh pr merge 5 --match-head-commit abc1234"),
            ("ask-merge-repo-merge", "gh pr merge 5 --repo o/r --merge"),
            (
                "ask-merge-repo-squash",
                "gh pr merge 5 --repo o/r --squash --delete-branch",
            ),
            (
                "ask-merge-squash-sha",
                "gh pr merge 5 --squash --match-head-commit abc1234def",
            ),
            (
                "ask-merge-all",
                "gh pr merge 5 --repo o/r --squash --delete-branch --match-head-commit abc1234def",
            ),
        ] {
            assert_ask_for_cos_deny_for_rest(tag, cmd);
        }
    }

    /// Every other merge shape keeps the 193bd14 hard block: the three
    /// reviews' repros, the chained and quoted lines that used to ask, and
    /// every non-canonical spelling.
    #[test]
    fn ask_tier_merge_every_other_shape_stays_blocked_for_cos() {
        let r = rig("ask-merge-block", Who::Cos);
        for cmd in [
            // F1: a dropped value must not hide a literal bypass flag.
            "gh pr merge 5 --admin",
            "gh pr merge 5 --body '' --admin",
            "gh pr merge 5 --body \"\" --auto",
            "gh pr merge 5 -b '' --admin",
            "gh pr merge 5 --body --admin",
            "gh pr merge 5 --subject \"$(cat s)\" --admin",
            "gh pr merge 5 -A \"$(git config user.email)\" --auto",
            "gh pr merge 5 --match-head-commit $(git rev-parse HEAD) --admin",
            "gh pr merge 5 -b\"$(cat m)\" --admin",
            "gh pr merge 5 --body <(cat m) --admin",
            "gh pr merge --admin=false 5",
            // F2: an argv the tokenizer cannot see.
            "gh pr merge 5 $(echo --admin)",
            "gh pr merge 5 `echo --admin`",
            "gh pr merge $(echo 5 --auto)",
            "gh pr merge 5 --body ''",
            "gh pr merge 5 --body-file <(echo notes)",
            "echo --admin | xargs gh pr merge 5",
            "xargs gh pr merge 5",
            "parallel gh pr merge 5",
            "xargs -n1 gh pr merge 5 --squash",
            // Flags and spellings the canonical line does not hold.
            "gh pr merge 5 --auto",
            "gh pr merge --auto 5",
            "gh pr merge 5 --auto=true",
            "gh pr merge 5 --disable-auto",
            "gh pr --auto merge 5",
            "gh pr merge 5 --bogus",
            "gh pr merge -Z 5",
            "gh pr merge $FLAG 5",
            "gh pr merge",
            "gh pr merge 5 --body hi",
            "gh pr merge 5 --body-file notes.txt",
            "gh pr merge 5 -A me@x",
            "gh pr merge 5 -sd",
            "gh pr merge 5 --match-head-commit=abc1234",
            "gh pr merge 5 --match-head-commit abc123",
            "gh pr merge 5 --squash --repo o/r",
            "gh pr merge 5 --delete-branch --squash",
            "gh pr merge 5 --repo o/r --squash --delete-branch --match-head-commit abc1234def extra",
            // N1: a repo value that reads like a bypass flag is not a repo.
            "gh pr merge 5 -R --admin/x",
            "gh pr merge 5 --repo --auto/x --squash",
            "gh pr merge 5 -R -/- --squash",
            "gh pr merge 5 -R ../.. --squash",
            // N2: whitespace, control and non-ASCII near-misses.
            "gh pr merge 5 ",
            " gh pr merge 5",
            "gh pr merge 5\t",
            "gh\tpr\tmerge\t5",
            "gh pr merge 5\r",
            "gh pr merge 5\n",
            "gh pr merge 5\n--admin",
            "gh pr merge 5\r\n--admin",
            "gh pr merge  5",
            "gh pr merge 5\u{A0}--admin",
            "gh pr merge \u{0665}",
            "gh pr merge \u{FF15}",
            "gh pr merge -R o/r 5",
            "gh pr merge --repo o/r 5 --squash",
            "gh -R o/r pr merge 5",
            "gh --repo o/r pr merge 5",
            "gh pr --repo o/r merge 5",
            "gh pr list merge",
            "gh pr view --json merge",
            // Chained, quoted and substituted lines that used to ask.
            "cd /tmp/x && gh pr merge 5 --squash",
            "gh pr merge 5 --squash && echo done",
            "x=$(echo); gh pr merge 5 --squash",
            "gh pr merge 5 --squash --subject 'feat: x' --body 'y z'",
            "gh pr merge https://github.com/o/r/pull/5 --rebase",
            "gh pr merge 5 -R o/r -s -d",
            // N1: a wrapper in front of gh.
            "env gh pr merge 5 $(echo --admin)",
            "command gh pr merge 5 $(echo --admin)",
            "exec gh pr merge 5 $(echo --admin)",
            "timeout 60 gh pr merge 5 $(echo --admin)",
            "sudo gh pr merge 5 `echo --admin`",
            "nice gh pr merge 5 <(echo x)",
            // N2: a quoted or escaped verb.
            "gh pr m'erge' 5 $(echo --admin)",
            "gh pr m\"erge\" 5 $(echo --admin)",
            "gh pr me\\rge 5 $(echo --admin)",
            "gh pr mer\\\nge 5 $(echo --admin)",
            "gh pr m'erge' 5 `echo --auto`",
            // N3: brace expansion.
            "gh pr merge {--admin,5}",
            "gh pr merge 5 {--auto,}",
            // N4/N5: find -exec, globs, a body starting with `-`.
            "find . -name x -exec gh pr merge 5 {} +",
            "find . -exec gh pr merge {} \\;",
            "gh pr merge 5 *",
            "gh pr merge 5 [-]-admin",
            "gh pr merge 5 --squash --body '- bullet one'",
            // R1: comments and heredoc bodies.
            "true # don't\ngh pr merge 5 $(echo --admin)",
            "echo hi # it's\ngh pr merge 5 `echo --auto`",
            "x=$(echo hi # it's\n); gh pr merge 5 $(echo --admin)",
            "cat <<EOF\nit's\nEOF\ngh pr merge 5 $(echo --admin)",
            // R2: a substitution inside a double-quoted -c script.
            "env sh -c \"gh pr merge 5 $(echo --admin)\"",
            "bash -c \"gh pr merge 5 $(echo --admin)\"",
            "sh -c \"gh pr merge 5 `echo --auto`\"",
            // R3: a shell function named gh.
            "gh() { command gh \"$@\" --admin; }; gh pr merge 5",
            "gh() {\n command gh \"$@\" --admin\n}\ngh pr merge 5 --squash",
            "function gh { command gh \"$@\" --auto; }; gh pr merge 5",
        ] {
            assert!(is_block(&r.bash(cmd)), "{cmd:?}");
        }
    }

    /// F3: the gate asks for a merge at every repo/lane tier and with a full
    /// grant; only the daemon's `pr.merge` door keeps the tier math.
    #[test]
    fn ask_tier_merge_asks_at_every_tier_and_grant() {
        let r = rig("ask-merge-tier", Who::Cos);
        let dir = r.ws.root().join("merge-repo");
        std::fs::create_dir_all(&dir).unwrap();
        let key = tier::repo_key(&dir);
        let hook = |cmd: &str| {
            json!({
                "tool_name": "Bash",
                "tool_input": {"command": cmd},
                "cwd": dir.to_string_lossy(),
            })
        };
        assert!(is_ask(&r.run(hook("gh pr merge 5"))), "unlisted repo");
        let c = qq_core::db::open(&r.ws.db_path()).unwrap();
        let set = |table: &str, col: &str, word: &str| {
            c.execute(
                &format!(
                    "INSERT OR REPLACE INTO {table} ({col}, tier, set_ms, set_by) VALUES (?1, ?2, 1, 'cos')"
                ),
                params![key, word],
            )
            .unwrap();
        };
        for word in ["low", "medium"] {
            set("repo_tier", "repo", word);
            set("plan_lane", "lane", word);
            assert!(
                is_ask(&r.run(hook("gh pr merge 5"))),
                "repo and lane {word}"
            );
            if word == "low" {
                // The rows are live: a release at low tier is allowed.
                assert_eq!(
                    r.run(hook("gh release create v1 --notes x")),
                    GateVerdict::Allow
                );
            }
        }
        c.execute(
            "INSERT OR REPLACE INTO grant_full (repo, granted_ms, granted_by) VALUES (?1, 1, 'cos')",
            params![key],
        )
        .unwrap();
        assert!(is_ask(&r.run(hook("gh pr merge 5"))), "full grant");
        assert!(
            is_ask(&r.run(hook("gh pr merge 5 --squash --delete-branch"))),
            "granted squash"
        );
    }

    #[test]
    fn ask_tier_reading_tags_is_not_a_hit() {
        let r = rig("ask-tag-read", Who::Down);
        for ok in [
            "git tag",
            "git tag -l",
            "git tag --list 'v*'",
            "git tag --contains abc",
            "gh release list",
            "gh release view v1",
        ] {
            assert_eq!(r.bash(ok), GateVerdict::Allow, "{ok}");
        }
    }

    #[test]
    fn ask_tier_branch_delete_local_and_remote() {
        assert_ask_for_cos_deny_for_rest("ask-bd", "git branch -D feat");
        assert_ask_for_cos_deny_for_rest("ask-bd2", "git branch -d feat/main-x");
        assert_ask_for_cos_deny_for_rest("ask-bd3", "git branch --delete feat");
        assert_ask_for_cos_deny_for_rest("ask-bdr", "git push origin --delete feat");
        assert_ask_for_cos_deny_for_rest("ask-bdr2", "git push origin :feat");
    }

    #[test]
    fn ask_tier_delete_of_main_stays_blocked_for_cos() {
        let r = rig("ask-bd-main", Who::Cos);
        for cmd in [
            "git branch -D main",
            "git push origin --delete main",
            "git push origin :master",
            "git push origin x --delete main",
        ] {
            assert!(is_block(&r.bash(cmd)), "{cmd}");
        }
    }

    #[test]
    fn ask_tier_raw_push_stays_blocked_for_cos() {
        let r = rig("ask-raw", Who::Cos);
        for cmd in [
            "git push origin feat",
            "git push origin main",
            "git push --force origin feat",
        ] {
            assert!(is_block(&r.bash(cmd)), "{cmd}");
        }
    }

    #[test]
    fn ask_tier_branch_move_of_trunk() {
        for cmd in [
            "git branch -m main old-main",
            "git branch -M master old",
            "git branch -f main abc123",
            "git branch --move main x",
            "git branch --force master HEAD~1",
            "git branch -m feat main",
        ] {
            assert_ask_for_cos_deny_for_rest("ask-bm", cmd);
        }
        let r = rig("ask-bm-ok", Who::Down);
        for ok in [
            "git branch -m feat feat2",
            "git branch -f feat abc",
            "git branch feat",
        ] {
            assert_eq!(r.bash(ok), GateVerdict::Allow, "{ok}");
        }
    }

    #[test]
    fn ask_tier_history_rewrite_on_shared_refs() {
        for cmd in [
            "git filter-branch --all",
            "git filter-repo --path x",
            "git update-ref -d refs/heads/main",
            "git update-ref refs/remotes/origin/main abc",
            "git update-ref refs/tags/v1 abc",
        ] {
            assert_ask_for_cos_deny_for_rest("ask-hr", cmd);
        }
        let r = rig("ask-hr-ok", Who::Down);
        assert_eq!(
            r.bash("git update-ref refs/heads/feat abc"),
            GateVerdict::Allow
        );
    }

    #[test]
    fn ask_tier_branch_merged_is_low() {
        let git = |dir: &std::path::Path, args: &[&str]| {
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
        };
        let ws = TestWorkspace::new("ask-merged");
        let up = ws.root().join("up");
        let cl = ws.root().join("cl");
        std::fs::create_dir_all(&up).unwrap();
        git(&up, &["init", "-q", "-b", "main"]);
        git(&up, &["commit", "-q", "--allow-empty", "-m", "a"]);
        git(ws.root(), &["clone", "-q", up.to_str().unwrap(), "cl"]);
        git(&cl, &["branch", "merged"]);
        git(&cl, &["checkout", "-q", "-b", "unmerged"]);
        git(&cl, &["commit", "-q", "--allow-empty", "-m", "b"]);
        git(&cl, &["checkout", "-q", "main"]);
        assert_eq!(
            tier::branch_delete_action(&cl, "merged"),
            Action::BranchDeleteMerged
        );
        assert_eq!(
            tier::branch_delete_action(&cl, "unmerged"),
            Action::BranchDeleteUnmerged
        );
        assert_eq!(
            tier::branch_delete_action(&cl, "nonexistent"),
            Action::BranchDeleteUnmerged
        );
        assert_eq!(
            tier::branch_delete_action(ws.root(), "merged"),
            Action::BranchDeleteUnmerged
        );
    }

    #[test]
    fn ask_tier_hook_output_is_pre_tool_use_ask_json() {
        let out = qq_core::authority::gate_ask_json("needs anh iu");
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["hookSpecificOutput"]["hookEventName"], "PreToolUse");
        assert_eq!(v["hookSpecificOutput"]["permissionDecision"], "ask");
        assert_eq!(
            v["hookSpecificOutput"]["permissionDecisionReason"],
            "needs anh iu"
        );
    }

    #[test]
    fn ask_tier_deny_says_report_to_the_cos() {
        {
            let r = rig("ask-msg", Who::Worker);
            match r.bash("git tag v1") {
                GateVerdict::Block(m) => assert!(m.contains("report to the CoS"), "{m}"),
                v => panic!("{v:?}"),
            }
        }
        let r = rig("ask-msg-down", Who::Down);
        match r.bash("git tag v1") {
            GateVerdict::Block(m) => assert!(m.contains("daemon"), "{m}"),
            v => panic!("{v:?}"),
        }
    }

    // ---- gate_bypass_* (one per #297 spelling, through the caller verdict) ----

    #[test]
    fn gate_bypass_alias_push() {
        for cmd in [
            "git config alias.p push",
            "git config alias.p '!git push'",
            "git -c alias.p=push status",
            "git -c ALIAS.p=push status",
        ] {
            assert_ask_for_cos_deny_for_rest("gb-alias", cmd);
        }
    }

    #[test]
    fn gate_bypass_gh_api_ref_write_off_main() {
        for cmd in [
            "gh api -X PATCH repos/o/r/git/refs/heads/feat -f sha=abc",
            "gh api -X DELETE repos/o/r/git/refs/heads/feat",
            "gh api -X POST repos/o/r/git/refs -f ref=refs/heads/feat -f sha=abc",
        ] {
            assert_ask_for_cos_deny_for_rest("gb-ref", cmd);
        }
    }

    #[test]
    fn gate_bypass_gh_api_ref_write_on_main_is_denied_for_cos() {
        let r = rig("gb-ref-main", Who::Cos);
        for cmd in [
            "gh api -X PATCH repos/o/r/git/refs/heads/main -f sha=abc",
            "gh api -X DELETE repos/o/r/git/refs/heads/master",
            "gh api -X POST repos/o/r/git/refs -f ref=refs/heads/main -f sha=abc",
        ] {
            assert!(is_block(&r.bash(cmd)), "{cmd}");
        }
    }

    #[test]
    fn gate_bypass_gh_api_contents_write() {
        assert_ask_for_cos_deny_for_rest(
            "gb-contents",
            "gh api -X PUT repos/o/r/contents/a.md -f message=m -f content=eA== -f branch=feat",
        );
        // No branch: the write lands on the default branch.
        let r = rig("gb-contents-main", Who::Cos);
        for cmd in [
            "gh api -X PUT repos/o/r/contents/a.md -f message=m -f content=eA==",
            "gh api -X PUT repos/o/r/contents/a.md -f message=m -f branch=main",
        ] {
            assert!(is_block(&r.bash(cmd)), "{cmd}");
        }
    }

    #[test]
    fn gate_bypass_auto_merge_and_merge_spellings_stay_blocked_for_cos() {
        let r = rig("gb-merge", Who::Cos);
        for cmd in [
            "gh api graphql -f query='mutation { enablePullRequestAutoMerge(input:{pullRequestId:\"x\"}) { clientMutationId } }'",
            "gh api graphql -f query='mutation { mergePullRequest(input:{pullRequestId:\"x\"}) { clientMutationId } }'",
            "gh api -X PUT repos/o/r/pulls/1/merge",
        ] {
            assert!(is_block(&r.bash(cmd)), "{cmd}");
        }
    }

    #[test]
    fn gate_bypass_graphql_query_from_file() {
        assert_ask_for_cos_deny_for_rest("gb-qfile", "gh api graphql -F query=@auto.graphql");
    }

    #[test]
    fn gate_bypass_reads_stay_allowed() {
        let r = rig("gb-reads", Who::Down);
        for ok in [
            "git config alias.st status",
            "gh api repos/o/r/git/refs/heads/main",
            "gh api graphql -f query='{ viewer { login } }'",
        ] {
            assert_eq!(r.bash(ok), GateVerdict::Allow, "{ok}");
        }
    }

    // ---- instruction_file_gate_* (R5) ----

    fn file_case(tag: &str, name: &str) {
        {
            let r = rig(&format!("{tag}-cos"), Who::Cos);
            let live = r.ws.root().join(name);
            std::fs::write(&live, "x").unwrap();
            let live = live.to_str().unwrap();
            for tool in ["Write", "Edit", "MultiEdit"] {
                assert!(is_ask(&r.edit(tool, live)), "cos {tool} {name} must ask");
            }
            // `..` and a relative path with the hook's cwd resolve to the same file.
            let dotted = format!("{}/sub/../{name}", r.ws.root().display());
            assert!(is_ask(&r.edit("Write", &dotted)), "{dotted}");
            let rel = r.run(json!({
                "tool_name": "Edit", "cwd": r.ws.root().to_str().unwrap(),
                "tool_input": {"file_path": name}
            }));
            assert!(is_ask(&rel), "relative {name}");
            // A symlink to the live file is the live file.
            let link = r.ws.root().join(format!("link-{name}"));
            std::os::unix::fs::symlink(live, &link).unwrap();
            assert!(
                is_ask(&r.edit("Write", link.to_str().unwrap())),
                "symlink {name}"
            );
        }
        for (who, n) in [
            (Who::Worker, "w"),
            (Who::Lead, "l"),
            (Who::Unmapped, "u"),
            (Who::Down, "d"),
        ] {
            let r = rig(&format!("{tag}-{n}"), who);
            let live = r.ws.root().join(name);
            assert!(
                is_block(&r.edit("Write", live.to_str().unwrap())),
                "{n} {name} must deny"
            );
            assert!(
                is_block(&r.edit("Edit", live.to_str().unwrap())),
                "{n} {name} must deny"
            );
        }
    }

    #[test]
    fn instruction_file_gate_agents_md() {
        file_case("ifg-agents", "AGENTS.md");
    }
    #[test]
    fn instruction_file_gate_identity_md() {
        file_case("ifg-ident", "identity.md");
    }

    #[test]
    fn instruction_file_gate_only_the_live_paths() {
        // Same basename elsewhere (a template, a repo AGENTS.md, a worktree) is
        // engine code workers edit under review; other live files are free (USER.md and
        // memory/rules.md are the exception: see tests/memory_write.rs).
        for who in [Who::Worker, Who::Cos] {
            let r = rig("ifg-scope", who);
            let root = r.ws.root();
            for p in [
                root.join("crates/qq-core/templates/workspace/AGENTS.md"),
                root.join("projects/x/AGENTS.md"),
                root.join("runs/AGENTS.md"),
                root.join("notes.md"),
                std::path::PathBuf::from("/tmp/elsewhere/identity.md"),
            ] {
                assert_eq!(
                    r.edit("Write", p.to_str().unwrap()),
                    GateVerdict::Allow,
                    "{p:?}"
                );
            }
        }
    }

    #[test]
    fn instruction_file_gate_other_tools_are_not_asked() {
        let r = rig("ifg-tools", Who::Down);
        let p = r.ws.root().join("AGENTS.md");
        let v =
            r.run(json!({"tool_name": "Read", "tool_input": {"file_path": p.to_str().unwrap()}}));
        assert_eq!(v, GateVerdict::Allow);
        // A write with no readable path fails closed.
        assert!(is_block(
            &r.run(json!({"tool_name": "Write", "tool_input": {}}))
        ));
    }

    // ---- pane_input_gate_* (R14 gate half) ----

    const PANE_INPUT: &[&str] = &[
        "herdr pane send-keys w1:p1 enter",
        "herdr pane send-text w1:p1 hello",
        "herdr pane run w1:p1 'ls'",
        "herdr pane report-metadata p --source s --token grid_role=cos",
    ];

    #[test]
    fn pane_input_gate_denies_non_cos() {
        for cmd in PANE_INPUT {
            assert_denied_for_rest("pig", cmd);
        }
    }

    #[test]
    fn pane_input_gate_lets_cos_through() {
        let r = rig("pig-cos", Who::Cos);
        for cmd in PANE_INPUT {
            assert_eq!(r.bash(cmd), GateVerdict::Allow, "{cmd}");
        }
        // Spawn, move and close stay the daemon's, cos included.
        assert!(is_block(&r.bash("herdr pane split w1:p1")));
        assert!(is_block(&r.bash("herdr pane close w1:p1")));
    }

    #[test]
    fn pane_input_gate_qq_db_naming_commands() {
        for cmd in [
            "sqlite3 runs/qq.db 'delete from agent'",
            "cp /tmp/x.db runs/qq.db",
            "sed -i s/a/b/ runs/qq.db",
            "rm runs/qq.db",
        ] {
            assert_ask_for_cos_deny_for_rest("pig-db", cmd);
        }
        let r = rig("pig-db-ro", Who::Worker);
        for ok in [
            "sqlite3 -readonly runs/qq.db 'select 1'",
            "cat runs/qq.db",
            "ls -l runs/qq.db",
            "sqlite3 other.db 'select 1'",
        ] {
            assert_eq!(r.bash(ok), GateVerdict::Allow, "{ok}");
        }
    }
}
