//! T55: the protocol reader, observed through the file on a real repo's
//! default branch, the Lead and Peer prompt sections and the cap decision.

use std::path::Path;
use std::process::Command;

use qq_core::authority::protocol::{self, Protocol};
use qq_core::authority::{self, Decision, Role};
use qq_core::config;
use qq_core::db;
use qq_core::task::prompt::compose_prompt_body;
use qq_core::testkit::TestWorkspace;
use rusqlite::{params, Connection};

fn git(repo: &Path, args: &[&str]) {
    let o = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["-c", "user.name=t", "-c", "user.email=t@t"])
        .args(args)
        .output()
        .unwrap();
    assert!(
        o.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&o.stderr)
    );
}

fn repo(ws: &TestWorkspace, name: &str) -> std::path::PathBuf {
    let p = ws.root().join(name);
    std::fs::create_dir_all(&p).unwrap();
    git(&p, &["init", "-q", "-b", "main"]);
    git(&p, &["commit", "-q", "--allow-empty", "-m", "init"]);
    p
}

fn commit_protocol(p: &Path, text: &str) {
    std::fs::write(p.join(protocol::FILE), text).unwrap();
    git(p, &["add", protocol::FILE]);
    git(p, &["commit", "-q", "-m", "protocol"]);
}

fn doc(body: &str) -> String {
    format!("# Protocol\n\ntext\n\n```toml\n{body}\n```\n")
}

#[test]
fn protocol_read_from_default_branch() {
    let ws = TestWorkspace::new("proto-read");
    let r = repo(&ws, "r");
    // Absent: engine default.
    assert_eq!(protocol::read(&r).unwrap(), Protocol::default());
    commit_protocol(&r, &doc("worker_cap = 2\nnotes = \"ship small\""));
    let p = protocol::read(&r).unwrap();
    assert_eq!((p.worker_cap, p.notes.as_deref()), (2, Some("ship small")));
    // A feature branch and a dirty worktree do not count: only main does.
    git(&r, &["checkout", "-q", "-b", "feat"]);
    commit_protocol(&r, &doc("worker_cap = 7"));
    assert_eq!(protocol::read(&r).unwrap().worker_cap, 2);
    std::fs::write(r.join(protocol::FILE), doc("worker_cap = 6")).unwrap();
    assert_eq!(protocol::read(&r).unwrap().worker_cap, 2);
    // No block in the file: engine default.
    git(&r, &["checkout", "-q", "--", protocol::FILE]);
    git(&r, &["checkout", "-q", "main"]);
    commit_protocol(&r, "# no block here\n");
    assert_eq!(protocol::read(&r).unwrap(), Protocol::default());
    // A key that is not a protocol key is refused, not ignored.
    commit_protocol(&r, &doc("worker_cap = 2\nrequire_review = false"));
    assert!(protocol::read(&r).is_err());
    commit_protocol(&r, &doc("worker_cap = 0"));
    assert!(protocol::read(&r).is_err());
}

fn setup(ws: &TestWorkspace) -> Connection {
    let mut conn = db::open(&ws.db_path()).unwrap();
    let t = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/team.toml");
    config::seed(&mut conn, &std::fs::read_to_string(t).unwrap()).unwrap();
    conn.execute(
        "INSERT INTO project (slug, created_ms) VALUES ('alpha', 0)",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO agent (name, role, project_id, started_ms) VALUES ('lead-a', 'lead', 1, 1)",
        [],
    )
    .unwrap();
    conn
}

fn peers(conn: &Connection, n: usize) {
    for i in 0..n {
        conn.execute(
            "INSERT INTO agent (name, role, project_id, spawner_id, started_ms) VALUES (?1, 'peer', 1, 1, 1)",
            params![format!("p{i}")],
        )
        .unwrap();
    }
}

fn cap_decision(conn: &Connection, cap: &str) -> Decision {
    let p = protocol::parse(&doc(cap)).unwrap();
    protocol::check_worker_cap(conn, "lead-a", 1, &p).unwrap()
}

#[test]
fn worker_cap_default_and_ceiling() {
    let ws = TestWorkspace::new("proto-cap");
    let conn = setup(&ws);
    assert_eq!(protocol::DEFAULT_CAP, 4);
    assert_eq!(protocol::parse("no block").unwrap().worker_cap, 4);
    // A protocol cannot raise the cap past the ceiling.
    assert_eq!(
        protocol::parse(&doc("worker_cap = 99")).unwrap().worker_cap,
        8
    );

    // Default cap 4: the 5th spawn is refused and journaled.
    let dflt = Protocol::default();
    peers(&conn, 3);
    assert_eq!(
        protocol::check_worker_cap(&conn, "lead-a", 1, &dflt).unwrap(),
        Decision::Allowed
    );
    peers_named(&conn, "q", 1);
    match protocol::check_worker_cap(&conn, "lead-a", 1, &dflt).unwrap() {
        Decision::Refused(r) => assert!(r.reason.contains("worker cap 4"), "{}", r.reason),
        Decision::Allowed => panic!("spawn over the default cap was allowed"),
    }
    assert_eq!(authority::refusals(&conn).unwrap().len(), 1);

    // A protocol cap of 6 allows up to 6, refuses the 7th; 99 stops at the ceiling 8.
    peers_named(&conn, "r", 2);
    assert_eq!(
        cap_decision(&conn, "worker_cap = 6"),
        Decision::Refused(authority::Refusal {
            caller: "lead-a".into(),
            edge: "spawn".into(),
            target: "peer".into(),
            reason: "worker cap 6 reached (6 active)".into(),
        })
    );
    assert_eq!(cap_decision(&conn, "worker_cap = 99"), Decision::Allowed);
    peers_named(&conn, "s", 2);
    assert!(matches!(
        cap_decision(&conn, "worker_cap = 99"),
        Decision::Refused(_)
    ));
}

fn peers_named(conn: &Connection, prefix: &str, n: usize) {
    for i in 0..n {
        conn.execute(
            "INSERT INTO agent (name, role, project_id, spawner_id, started_ms) VALUES (?1, 'peer', 1, 1, 1)",
            params![format!("{prefix}{i}")],
        )
        .unwrap();
    }
}

#[test]
fn protocol_absent_from_peer_prompt() {
    let p = protocol::parse(&doc("worker_cap = 3\nnotes = \"SECRET-TACTIC\"")).unwrap();
    let lead = protocol::lead_prompt_section(Role::Lead, &p).unwrap();
    assert!(lead.contains("worker cap 3") && lead.contains("SECRET-TACTIC"));
    for role in [Role::Peer, Role::Supervisor, Role::Cos] {
        assert_eq!(protocol::lead_prompt_section(role, &p), None, "{role:?}");
    }
    // The Peer brief path carries no protocol text.
    let brief = compose_prompt_body("p1", "do the task");
    assert!(!brief.contains("SECRET-TACTIC") && !brief.contains(protocol::FILE));
    assert!(!brief.contains(&p.source));
}
