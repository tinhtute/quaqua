//! The review pair rule: same model refused and journaled, another model allowed.

use std::path::Path;

use qq_core::authority;
use qq_core::config;
use qq_core::db;
use qq_core::task::review_pair;

fn conn(tag: &str) -> rusqlite::Connection {
    let dir = std::env::temp_dir().join(format!("qq-review-pair-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let mut c = db::open(&dir.join(db::path::DB_FILE)).expect("open qq db");
    let team = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/team.toml"),
    )
    .unwrap();
    config::seed(&mut c, &team).expect("seed");
    c
}

fn add_kind(c: &rusqlite::Connection, name: &str, harness: &str, model: &str) {
    c.execute(
        "INSERT INTO kind (name, harness, model, can_review) VALUES (?1, ?2, ?3, 1)",
        [name, harness, model],
    )
    .unwrap();
}

#[test]
fn review_refused_on_same_model() {
    let c = conn("refused");
    // ni runs on claude, other on opencode: two harnesses, one model.
    add_kind(&c, "other", "opencode", "opus-5.5");
    let cause = review_pair::check(&c, "ni", "other").expect_err("same model is refused");
    assert!(cause.contains("'ni'") && cause.contains("'other'") && cause.contains("opus-5.5"));
    let rows = authority::refusals(&c).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].edge, "review");
    for part in ["'ni'", "'other'", "'opus-5.5'"] {
        assert!(rows[0].reason.contains(part), "{part} in {}", rows[0].reason);
    }
    // Same harness, same model, different spelling of case: refused too.
    add_kind(&c, "loud", "claude", " OPUS-5.5 ");
    assert!(review_pair::check(&c, "ni", "loud").is_err());
    assert_eq!(authority::refusals(&c).unwrap().len(), 2);
}

#[test]
fn review_allowed_same_harness_other_model() {
    let c = conn("allowed");
    // sude and tieude both run on pi, on different models.
    review_pair::check(&c, "sude", "tieude").expect("another model is allowed");
    assert!(authority::refusals(&c).unwrap().is_empty());
}
