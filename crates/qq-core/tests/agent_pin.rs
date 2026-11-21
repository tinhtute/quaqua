//! T52b: a spawn pins harness, model and effort on the agent row; a later
//! profile switch moves the next spawn only, never how a live worker is
//! stopped or fenced.

use std::path::Path;

use qq_core::authority::push::fence_of;
use qq_core::authority::Fence;
use qq_core::config;
use qq_core::db;
use qq_core::driver::Registry;
use qq_core::herdr::{FakeHerdr, HerdrClient};
use qq_core::task::cleanup::{self, OpsError};
use qq_core::task::spawn::{spawn_with, Ready, Role, SpawnSpec};
use rusqlite::Connection;

fn seeded(tag: &str) -> (std::path::PathBuf, Connection) {
    let dir = std::env::temp_dir().join(format!("qq-pin-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let mut conn = db::open(&dir.join(db::path::DB_FILE)).unwrap();
    let team = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/team.toml"),
    )
    .unwrap();
    config::seed(&mut conn, &team).unwrap();
    (dir, conn)
}

fn start(conn: &Connection, herdr: &FakeHerdr, name: &str, kind: &str, role: Role) {
    let pane = herdr.seed_pane("r-workers", None, &[]);
    let ready = Ready {
        polls: 1,
        poll_ms: 1,
        settle_polls: 1,
        settle_ms: 1,
    };
    let spec = SpawnSpec {
        name,
        kind,
        role,
        pane: &pane,
        project_id: None,
        spawner_id: None,
    };
    spawn_with(conn, herdr, &Registry::native(), &spec, 1, &ready).unwrap();
}

/// Every kind in `from` resolves to harness `to` from now on.
fn switch_profile(conn: &Connection, kind: &str, to: &str) {
    conn.execute("INSERT INTO profile (name) VALUES ('swap')", [])
        .unwrap();
    conn.execute(
        "INSERT INTO profile_kind (profile, kind, harness) VALUES ('swap', ?1, ?2)",
        [kind, to],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO setting (key, value_json) VALUES ('profile', '\"swap\"')",
        [],
    )
    .unwrap();
}

#[test]
fn agent_pin_survives_profile_switch() {
    let (_dir, conn) = seeded("switch");
    let herdr = FakeHerdr::new();

    // sude runs on pi; ni (a Lead) on claude, a native, fenced driver.
    start(&conn, &herdr, "w1", "sude", Role::Peer);
    start(&conn, &herdr, "l1", "ni", Role::Lead);
    let pin = |n: &str| -> (String, String, Option<String>) {
        conn.query_row(
            "SELECT harness, model, effort FROM agent WHERE name = ?1",
            [n],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap()
    };
    assert_eq!(
        pin("w1"),
        (
            "pi".into(),
            "google-antigravity/gemini-3.8-flash".into(),
            Some("high".into())
        )
    );
    assert_eq!(pin("l1").0, "claude");
    assert_eq!(fence_of(&conn, "l1"), Fence::Fenced);

    // A profile now sends both kinds elsewhere: sude -> claude (no stop keys),
    // ni -> opencode (generic, unfenced).
    switch_profile(&conn, "sude", "claude");
    conn.execute(
        "INSERT INTO profile_kind (profile, kind, harness) VALUES ('swap', 'ni', 'opencode')",
        [],
    )
    .unwrap();
    assert_eq!(
        config::resolve_spawn(&conn, "sude").unwrap().harness.name,
        "claude"
    );
    assert_eq!(
        config::resolve_spawn(&conn, "ni").unwrap().harness.name,
        "opencode"
    );

    // The running workers keep their spawn-time harness.
    herdr.set_status("w1", "working");
    herdr.idle_on_keys("w1", &["esc"]);
    cleanup::stop_worker(&conn, &herdr, "w1").unwrap();
    assert_eq!(
        herdr.keys().last(),
        Some(&("w1".to_string(), vec!["esc".to_string()]))
    );
    assert_eq!(fence_of(&conn, "l1"), Fence::Fenced);

    // A row from before the pin has no harness: it falls back to its kind.
    conn.execute(
        "INSERT INTO agent (name, role, kind, started_ms) VALUES ('old', 'peer', 'sude', 1)",
        [],
    )
    .unwrap();
    herdr
        .agent_start(
            "old",
            "pi",
            &herdr.seed_pane("r-workers", None, &[]),
            1,
            &[],
        )
        .unwrap();
    herdr.set_status("old", "working");
    assert_eq!(
        cleanup::stop_worker(&conn, &herdr, "old"),
        Err(OpsError::Unverified("claude".into()))
    );

    // The next spawn resolves the new profile and pins it.
    start(&conn, &herdr, "w2", "ni", Role::Peer);
    assert_eq!(pin("w2").0, "opencode");
}
