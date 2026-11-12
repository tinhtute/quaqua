//! `quaqua verify record` through the built binary: a merge with no verdict is
//! a G1 gap; recording a coordinator rerun at the merged head closes it.

use std::path::Path;
use std::process::{Command, Output};

fn git(repo: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .current_dir(repo)
        .args(["-c", "user.email=t@t", "-c", "user.name=t"])
        .args(args)
        .output()
        .expect("git");
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn quaqua(ws: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_quaqua"))
        .args(args)
        .env("HOME", ws)
        .env("QUAQUA_HOME", ws)
        .env("QUAQUA_RUNS_DIR", ws.join("runs"))
        .env("QUAQUA_TEST", "1")
        .output()
        .expect("spawn quaqua")
}

fn g1(ws: &Path) -> (u64, u64) {
    let out = quaqua(ws, &["status", "--metrics", "--json", "--since", "30d"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).expect("json");
    let j = &v["journal"];
    (
        j["merges_checked"].as_u64().expect("merges_checked"),
        j["merges_without_verdict"]
            .as_u64()
            .expect("merges_without_verdict"),
    )
}

#[test]
fn rerun_verdict_makes_g1_count_the_merge_as_verified() {
    let ws = std::env::temp_dir().join(format!("quaqua-verify-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&ws);
    std::fs::create_dir_all(ws.join("runs")).unwrap();
    git(&ws, &["init", "-q", "-b", "main"]);
    git(&ws, &["commit", "--allow-empty", "-qm", "init"]);
    git(&ws, &["checkout", "-qb", "quaqua/proj-integ"]);
    let mut heads = Vec::new();
    for w in ["wk1", "wk2"] {
        git(
            &ws,
            &[
                "checkout",
                "-qb",
                &format!("quaqua/{w}"),
                "quaqua/proj-integ",
            ],
        );
        git(&ws, &["commit", "--allow-empty", "-qm", w]);
        heads.push(git(&ws, &["rev-parse", "HEAD"]));
        git(&ws, &["checkout", "-q", "quaqua/proj-integ"]);
        git(
            &ws,
            &[
                "merge",
                "--no-ff",
                "-qm",
                &format!("merge {w}"),
                &format!("quaqua/{w}"),
            ],
        );
    }
    let head = heads[0].clone();
    // G1 reads the journal; give it one (empty) so metrics are reported.
    quaqua::engine::journal_sqlite::SqliteJournal::open(&ws.join("runs/quaqua.db")).unwrap();
    let repo = ws.to_str().unwrap();

    assert_eq!(g1(&ws), (2, 2), "merges with no verdict are not counted");

    // A failed rerun is a verdict but not a pass.
    let bad = quaqua(
        &ws,
        &[
            "verify",
            "record",
            "wk2",
            "--cmd",
            "cargo test x",
            "--level",
            "verifier-failed",
            "--repo",
            repo,
        ],
    );
    assert!(
        bad.status.success(),
        "{}",
        String::from_utf8_lossy(&bad.stderr)
    );
    assert_eq!(g1(&ws), (2, 2));

    let args = [
        "verify",
        "record",
        &head[..10],
        "--cmd",
        "cargo test x",
        "--level",
        "unit-test-verified",
        "--repo",
        repo,
    ];
    let ok = quaqua(&ws, &args);
    assert!(
        ok.status.success(),
        "{}",
        String::from_utf8_lossy(&ok.stderr)
    );
    assert!(String::from_utf8_lossy(&ok.stdout).contains(&head));
    assert_eq!(g1(&ws), (2, 1), "recorded rerun covers the merged head");

    // Idempotent: repeating adds nothing and still succeeds.
    let again = quaqua(&ws, &args);
    assert!(again.status.success());
    assert!(String::from_utf8_lossy(&again.stdout).contains("already recorded"));
    assert_eq!(g1(&ws), (2, 1));

    // Bad input is rejected without touching the journal.
    let unk = quaqua(
        &ws,
        &[
            "verify",
            "record",
            "wk1",
            "--cmd",
            "c",
            "--level",
            "great",
            "--repo",
            ws.to_str().unwrap(),
        ],
    );
    assert!(!unk.status.success());
    let nope = quaqua(
        &ws,
        &[
            "verify",
            "record",
            "no-such",
            "--cmd",
            "c",
            "--level",
            "review-clean",
            "--repo",
            ws.to_str().unwrap(),
        ],
    );
    assert!(!nope.status.success());

    let _ = std::fs::remove_dir_all(&ws);
}
