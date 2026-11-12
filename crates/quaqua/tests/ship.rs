use quaqua::ship::{
    resolve_ship_repo, ship, Approval, AssumeYes, ShipOps, ShipRequest, PR_SIGNATURE,
};
use std::cell::RefCell;
use std::path::Path;

struct Fake {
    ahead: u32,
    existing_pr: Option<String>,
    calls: RefCell<Vec<String>>,
}

impl Fake {
    fn new(ahead: u32) -> Self {
        Fake {
            ahead,
            existing_pr: None,
            calls: RefCell::new(vec![]),
        }
    }
    fn log(&self, s: String) {
        self.calls.borrow_mut().push(s);
    }
    fn calls(&self) -> Vec<String> {
        self.calls.borrow().clone()
    }
}

impl ShipOps for Fake {
    fn commits_ahead(&self, _: &Path, _: &str, _: &str) -> Result<u32, String> {
        Ok(self.ahead)
    }
    fn tip_subject(&self, _: &Path, _: &str) -> Result<String, String> {
        Ok("feat(ship): add ship".into())
    }
    fn push(&self, _: &Path, branch: &str) -> Result<(), String> {
        self.log(format!("push {branch}"));
        Ok(())
    }
    fn open_pr(&self, _: &Path, _: &str) -> Result<Option<String>, String> {
        Ok(self.existing_pr.clone())
    }
    fn create_pr(
        &self,
        _: &Path,
        branch: &str,
        base: &str,
        title: &str,
        body: &str,
    ) -> Result<String, String> {
        self.log(format!("pr_create {branch} {base} {title} || {body}"));
        Ok("https://example.test/pr/1".into())
    }
}

struct Deny;
impl Approval for Deny {
    fn approve(&self, _: &str) -> bool {
        false
    }
}

const COMPLETE: &str =
    "Outcome:         COMPLETE\nScope:           src/ship.rs\n\n## Blockers\nnone\n";

fn req(report: &str) -> ShipRequest<'_> {
    ShipRequest {
        worker: "w1",
        repo: Path::new("/repo"),
        base: "main",
        report,
    }
}

#[test]
fn ship_refuses_without_approval_and_touches_no_remote() {
    let ops = Fake::new(2);
    let err = ship(&ops, &Deny, &req(COMPLETE)).unwrap_err();
    assert_eq!(err.0, 1);
    assert!(ops.calls().is_empty());
}

#[test]
fn ship_refuses_non_complete_report() {
    for report in [
        "Outcome: BLOCKED\n",
        "Outcome: NO_CHANGES\n",
        "no outcome here\n",
    ] {
        let ops = Fake::new(2);
        assert_eq!(
            ship(&ops, &AssumeYes, &req(report)).unwrap_err().0,
            1,
            "{report}"
        );
        assert!(ops.calls().is_empty());
    }
}

#[test]
fn ship_refuses_zero_commits_ahead() {
    let ops = Fake::new(0);
    assert_eq!(ship(&ops, &AssumeYes, &req(COMPLETE)).unwrap_err().0, 1);
    assert!(ops.calls().is_empty());
}

#[test]
fn ship_pushes_then_opens_pr_and_never_merges() {
    let ops = Fake::new(3);
    let url = ship(&ops, &AssumeYes, &req(COMPLETE)).unwrap();
    assert_eq!(url, "https://example.test/pr/1");
    let calls = ops.calls();
    assert_eq!(calls.len(), 2, "{calls:?}");
    assert_eq!(calls[0], "push quaqua/w1");
    assert!(
        calls[1].starts_with("pr_create quaqua/w1 main feat(ship): add ship ||"),
        "{}",
        calls[1]
    );
    assert!(calls[1].contains("Outcome:         COMPLETE"));
    assert!(calls[1].trim_end().ends_with(PR_SIGNATURE));
    assert!(calls.iter().all(|c| !c.contains("merge")));
}

#[test]
fn ship_rerun_reuses_open_pr() {
    let mut ops = Fake::new(1);
    ops.existing_pr = Some("https://example.test/pr/9".into());
    assert_eq!(
        ship(&ops, &AssumeYes, &req(COMPLETE)).unwrap(),
        "https://example.test/pr/9"
    );
    assert_eq!(ops.calls(), vec!["push quaqua/w1"]);
}

/// One `runs/telemetry.jsonl` worker row, as `worker cleanup` appends it.
fn telemetry_row(worker: &str, repo: &str) -> String {
    format!(
        "{{\"ts\":\"2026-11-12T15:06:40+07:00\",\"rec\":\"worker\",\"name\":\"{worker}\",\
         \"kind\":\"sude\",\"harness\":\"claude\",\"owner\":\"cos\",\"pipeline\":null,\
         \"repo\":\"{repo}\",\"prompts\":1,\"exit\":null,\"wall_sec\":217,\
         \"budget_sec\":2700,\"usd\":null,\"usd_budget\":null,\"tokens\":null,\
         \"spend_src\":\"claude-tokens\",\"model\":null}}\n"
    )
}

/// `worker cleanup` deletes `runs/<w>.repo`, so a ship after cleanup must find
/// the repo in the durable telemetry row instead of demanding `--repo`.
#[test]
fn ship_after_cleanup_resolves_repo_without_repo_flag() {
    let runs = std::env::temp_dir().join(format!("quaqua-ship-postcleanup-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&runs);
    std::fs::create_dir_all(&runs).unwrap();
    std::fs::write(runs.join("w1.md"), COMPLETE).unwrap();
    std::fs::write(
        runs.join("telemetry.jsonl"),
        telemetry_row("w1", "~/Lab/quaqua"),
    )
    .unwrap();
    assert!(
        !runs.join("w1.repo").exists(),
        "cleanup removed the spawn record"
    );

    let out = std::process::Command::new(env!("CARGO_BIN_EXE_quaqua"))
        .args(["ship", "w1"])
        .env("QUAQUA_RUNS_DIR", &runs)
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "{stderr}");
    assert!(
        !stderr.contains(".repo"),
        "the repo must resolve without --repo: {stderr}"
    );
    assert!(
        stderr.contains("no TTY"),
        "ship must reach the approval gate: {stderr}"
    );
    let _ = std::fs::remove_dir_all(&runs);
}

/// The resolved path must be the repo itself, not just "some path": the spawn
/// record wins while live, the newest telemetry row answers after cleanup.
#[test]
fn resolve_ship_repo_prefers_spawn_record_then_newest_telemetry_row() {
    let runs = std::env::temp_dir().join(format!("quaqua-ship-resolve-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&runs);
    std::fs::create_dir_all(&runs).unwrap();
    let home = "/srv/worker";

    // No evidence at all, and a row for another worker, both refuse.
    std::fs::write(
        runs.join("telemetry.jsonl"),
        telemetry_row("w2", "~/Lab/other"),
    )
    .unwrap();
    let err = resolve_ship_repo(&runs, "w1", home).expect_err("no evidence must refuse");
    assert!(
        err.contains("w1.repo") && err.contains("telemetry.jsonl"),
        "{err}"
    );

    // Cleanup appended the row: the repo survives the spawn record.
    std::fs::write(
        runs.join("telemetry.jsonl"),
        telemetry_row("w2", "~/Lab/other") + &telemetry_row("w1", "~/Lab/quaqua"),
    )
    .unwrap();
    assert_eq!(
        resolve_ship_repo(&runs, "w1", home).unwrap(),
        std::path::PathBuf::from("/srv/worker/Lab/quaqua")
    );

    // A reused worker name: the newest cleanup is the current repo.
    std::fs::write(
        runs.join("telemetry.jsonl"),
        telemetry_row("w1", "~/Lab/quaqua") + &telemetry_row("w1", "~/Work/newer"),
    )
    .unwrap();
    assert_eq!(
        resolve_ship_repo(&runs, "w1", home).unwrap(),
        std::path::PathBuf::from("/srv/worker/Work/newer")
    );

    // A live worker's spawn record outranks any older telemetry row.
    std::fs::write(runs.join("w1.repo"), "/repo/live\n").unwrap();
    assert_eq!(
        resolve_ship_repo(&runs, "w1", home).unwrap(),
        std::path::PathBuf::from("/repo/live")
    );

    // A row without a repo is not evidence.
    std::fs::remove_file(runs.join("w1.repo")).unwrap();
    std::fs::write(runs.join("telemetry.jsonl"), telemetry_row("w1", "")).unwrap();
    assert!(resolve_ship_repo(&runs, "w1", home).is_err());
    let _ = std::fs::remove_dir_all(&runs);
}

#[test]
fn ship_cli_refuses_without_tty_or_yes() {
    let runs = std::env::temp_dir().join(format!("quaqua-ship-cli-{}", std::process::id()));
    std::fs::create_dir_all(&runs).unwrap();
    std::fs::write(runs.join("w1.md"), COMPLETE).unwrap();
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_quaqua"))
        .args(["ship", "w1", "--repo", "/nonexistent"])
        .env("QUAQUA_RUNS_DIR", &runs)
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    let _ = std::fs::remove_dir_all(&runs);
}
