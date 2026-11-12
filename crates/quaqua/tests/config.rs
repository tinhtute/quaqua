//! R1 integration tests: `quaqua config validate` and `quaqua config get` against
//! the repo-root `team.toml` and the invalid fixtures. Each case shells out to the
//! built binary (CARGO_BIN_EXE_quaqua), so the CLI wiring is under test, not just
//! the library.

use std::path::PathBuf;
use std::process::{Command, Output};

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn repo_team() -> String {
    repo_root()
        .join("templates/workspace/team.toml")
        .to_string_lossy()
        .into_owned()
}

fn run(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_quaqua"))
        .args(args)
        .output()
        .expect("failed to spawn the quaqua binary")
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

#[test]
fn config_repo_team_toml_validates_ok() {
    let out = run(&["config", "validate", "--file", &repo_team()]);
    assert!(out.status.success(), "validate failed: {}", stderr(&out));
    assert_eq!(stdout(&out).trim(), "ok");
}

#[test]
fn config_repo_kinds_carry_model_and_effort() {
    // team.toml is the only source of harness/model/effort: the repo config must
    // validate on its own (no models.md drift check anymore).
    let out = run(&["config", "validate", "--file", &repo_team()]);
    assert!(out.status.success(), "validate failed: {}", stderr(&out));
    let effort = run(&["config", "get", "kinds.tieude.effort", "--file", &repo_team()]);
    assert_eq!(stdout(&effort).trim(), "high");
    let fallback = run(&["config", "get", "kinds.tieude.fallback", "--file", &repo_team()]);
    assert_eq!(stdout(&fallback).trim(), "sude");
    let sude_fallback = run(&["config", "get", "kinds.sude.fallback", "--file", &repo_team()]);
    assert_eq!(stdout(&sude_fallback).trim(), "suhuynh");
}

/// The migration plan's Rule -> key table is the record of which source rule
/// became which `team.toml` key: one row per rule, per-source counts exact, and
/// no row silently dropped (each maps to a key or says `prose`).
#[test]
fn config_rule_key_table_covers_every_source_rule() {
    let plan = repo_root().join("docs/research/herdr-grid-rust-migration-plan.md");
    let body = std::fs::read_to_string(&plan).expect("read the migration plan");
    let table: Vec<&str> = body
        .lines()
        .skip_while(|l| !l.contains("rule-key-table:start"))
        .skip(1)
        .take_while(|l| !l.contains("rule-key-table:end"))
        .filter(|l| l.starts_with("| "))
        .collect();
    // The header row starts with "| "; the |---| separator does not.
    let rows = &table[1..];
    assert_eq!(
        rows.len(),
        40,
        "rule->key table has {} data rows, expected 40",
        rows.len()
    );

    for (source, want) in [
        ("| AGENTS.md", 13),           // 7 Rules + 6 Protocol
        ("| `worker-cap.sh`", 4),
        ("| `worker-timebox.md`", 11), // one per bullet
        ("| `models.md`", 10),
        ("| plan", 2),
    ] {
        let got = rows.iter().filter(|r| r.starts_with(source)).count();
        assert_eq!(got, want, "rows from {source}");
    }

    let unmapped: Vec<&&str> = rows
        .iter()
        .filter(|r| !r.contains('`') && !r.contains("prose"))
        .collect();
    assert!(
        unmapped.is_empty(),
        "{} rule->key row(s) map to neither a key nor prose: {unmapped:?}",
        unmapped.len()
    );
}

#[test]
fn config_base_fixture_validates_ok() {
    let base = fixtures_dir().join("base.toml");
    let out = run(&["config", "validate", "--file", &base.to_string_lossy()]);
    assert!(out.status.success(), "base fixture rejected: {}", stderr(&out));
}

/// Review round 2, finding 1: `--project X` must not re-check the other projects
/// (or the weights) against X's resolved cap.
#[test]
fn config_validate_project_does_not_recheck_other_projects() {
    let f = fixtures_dir().join("two-projects.toml");
    let f = f.to_string_lossy().into_owned();

    // plain validate and every project selection must pass
    for args in [
        vec!["config", "validate", "--file", f.as_str()],
        vec!["config", "validate", "--file", f.as_str(), "--project", "a"],
        vec!["config", "validate", "--file", f.as_str(), "--project", "b"],
        vec!["config", "validate", "--file", f.as_str(), "--project", "c"],
    ] {
        let out = run(&args);
        assert!(
            out.status.success(),
            "{args:?} rejected: {}",
            stderr(&out)
        );
    }

    // an unknown project is still an error
    let ghost = run(&[
        "config", "validate", "--file", &f, "--project", "ghost",
    ]);
    assert!(!ghost.status.success());
    assert!(stderr(&ghost).contains("unknown project 'ghost'"), "{}", stderr(&ghost));
}

#[test]
fn config_get_default_reviewer_is_a_list_one_per_line() {
    let out = run(&[
        "config",
        "get",
        "strategy.default_reviewer",
        "--file",
        &repo_team(),
    ]);
    assert!(out.status.success(), "get failed: {}", stderr(&out));
    assert_eq!(stdout(&out), "suhuynh\n");
}

#[test]
fn config_get_cap_applies_project_override() {
    let base = run(&[
        "config",
        "get",
        "limits.max_workers_per_orchestrator",
        "--file",
        &repo_team(),
    ]);
    assert_eq!(stdout(&base).trim(), "8");

    let zimage = run(&[
        "config",
        "get",
        "limits.max_workers_per_orchestrator",
        "--file",
        &repo_team(),
        "--project",
        "zimage",
    ]);
    assert!(zimage.status.success(), "override get failed: {}", stderr(&zimage));
    assert_eq!(stdout(&zimage).trim(), "3");
}

#[test]
fn config_lead_peer_cap_matches_the_slp() {
    let file = repo_team();

    // the SLP pair: builder sude (omp) and reviewer suhuynh (pi) differ by harness
    let differ = run(&["config", "get", "strategy.builder_reviewer_differ", "--file", &file]);
    assert_eq!(stdout(&differ).trim(), "harness");

    // a Lead spawns at most 4 Peers ...
    let peers = run(&["config", "get", "roles.lead.max_peers", "--file", &file]);
    assert!(peers.status.success(), "get failed: {}", stderr(&peers));
    assert_eq!(stdout(&peers).trim(), "4");

    // ... while the CoS keeps the global cap
    let cap = run(&[
        "config",
        "get",
        "limits.max_workers_per_orchestrator",
        "--file",
        &file,
    ]);
    assert_eq!(stdout(&cap).trim(), "8");

    // an older team.toml without the key loads and still resolves the default
    let base = fixtures_dir().join("base.toml");
    let old = run(&[
        "config",
        "get",
        "roles.lead.max_peers",
        "--file",
        &base.to_string_lossy(),
    ]);
    assert!(old.status.success(), "get failed: {}", stderr(&old));
    assert_eq!(stdout(&old).trim(), "4");
}

#[test]
fn config_get_can_review_and_kind_fields() {
    let can = run(&["config", "get", "kinds.suhuynh.can_review", "--file", &repo_team()]);
    assert_eq!(stdout(&can).trim(), "true");
    let cannot = run(&["config", "get", "kinds.tieude.can_review", "--file", &repo_team()]);
    assert_eq!(stdout(&cannot).trim(), "false");
    let harness = run(&["config", "get", "kinds.sude.harness", "--file", &repo_team()]);
    assert_eq!(stdout(&harness).trim(), "omp");
    let effort = run(&["config", "get", "kinds.sude.effort", "--file", &repo_team()]);
    assert_eq!(stdout(&effort).trim(), "high");
    let spawn = run(&["config", "get", "kinds.truonglao.spawn", "--file", &repo_team()]);
    assert_eq!(stdout(&spawn).trim(), "auto");
}

#[test]
fn config_get_unknown_key_fails_clearly() {
    let out = run(&["config", "get", "nope.nope", "--file", &repo_team()]);
    assert!(!out.status.success());
    assert!(stderr(&out).contains("unknown key 'nope.nope'"), "stderr: {}", stderr(&out));
}

#[test]
fn config_get_unknown_project_fails_clearly() {
    let out = run(&[
        "config",
        "get",
        "limits.max_workers_per_orchestrator",
        "--file",
        &repo_team(),
        "--project",
        "ghost",
    ]);
    assert!(!out.status.success());
    assert!(stderr(&out).contains("unknown project 'ghost'"), "stderr: {}", stderr(&out));
}

/// Each invalid fixture is rejected, exits non-zero, and the message names the
/// rule that fired.
#[test]
fn config_invalid_fixtures_are_rejected() {
    let cases: &[(&str, &str)] = &[
        ("cap-over.toml", "over the cap"),
        ("bad-weights.toml", "exceeds cap"),
        ("project-cap-over.toml", "exceeds global cap"),
        ("unknown-kind.toml", "unknown kind 'ghost'"),
        ("unknown-class.toml", "unknown role-class 'ghostrole'"),
        ("unknown-role.toml", "unknown role 'ghost'"),
        ("same-harness.toml", "share harness"),
        ("reviewer-cannot-review.toml", "can_review = false"),
        ("missing-effort.toml", "effort is missing"),
        ("bad-effort.toml", "does not take effort 'banana'"),
        ("bad-opencode-variant.toml", "does not take effort 'medium'"),
        ("bad-opencode-openrouter-variant.toml", "does not take effort 'medium'"),
        ("unknown-opencode-model.toml", "is not in allowlist (fix: add model and variants to team.toml [opencode_models])"),
        ("fallback-missing.toml", "fallback 'ghost' is not a kind"),
        ("fallback-self.toml", "fallback names itself"),
        ("fallback-cannot-review.toml", "cannot review"),
        ("bad-schema.toml", "unsupported schema_version"),
        ("integ-branch.toml", "{slug}"),
        ("differ-policy.toml", "unknown differ policy"),
        ("typo-key.toml", "unknown field `can_reveiw`"),
        ("typo-section.toml", "unknown field `project`"),
        ("empty-reviewer.toml", "default_reviewer is empty"),
        ("project-cap-zero.toml", "is below 1"),
        ("lead-max-peers-over.toml", "roles.lead.max_peers 9 must be in 1..=8"),
        ("lead-max-peers-zero.toml", "roles.lead.max_peers 0 must be in 1..=8"),
        ("cos-max-peers.toml", "role 'cos' takes no max_peers"),
        ("bad-timer-duration.toml", "timers.budget_default"),
        ("bad-timer-nudge.toml", "timers.nudge_at"),
        ("bad-timer-grace.toml", "timers.grace"),
        ("unknown-spawn.toml", "unknown spawn policy 'sometimes'"),
        ("kill-on-claude.toml", "kind 'ni': on_cost_cap = \"kill\" is not allowed on harness 'claude'"),
        ("kill-on-pi.toml", "kind 'ni': on_cost_cap = \"kill\" is not allowed on harness 'pi'"),
        ("kill-fallback-claude.toml", "kind 'ni': on_cost_cap = \"kill\" is not allowed on fallback 'rv'"),
    ];

    for (name, needle) in cases {
        let path = fixtures_dir().join("invalid").join(name);
        let path_s = path.to_string_lossy().into_owned();
        let out = run(&["config", "validate", "--file", path_s.as_str()]);
        let err = stderr(&out);
        assert!(
            !out.status.success(),
            "{name}: expected rejection, got success (stdout={:?})",
            stdout(&out)
        );
        assert!(
            err.contains(needle),
            "{name}: stderr {err:?} does not contain {needle:?}"
        );
    }
}

#[test]
fn config_get_purposes() {
    let out = run(&["config", "get", "purposes.review", "--file", &repo_team()]);
    assert!(out.status.success(), "get failed: {}", stderr(&out));
    assert_eq!(stdout(&out).trim(), "suhuynh");
}

#[test]
fn config_get_opencode_models() {
    let out = run(&["config", "get", "opencode_models.deepseek-v4.1-flash", "--file", &repo_team()]);
    assert!(out.status.success(), "get failed: {}", stderr(&out));
    assert!(stdout(&out).contains("max"));
}

#[test]
fn config_opencode_models_table_merges_over_defaults() {
    let temp = std::env::temp_dir().join(format!("quaqua-test-models-merge-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&temp);
    let toml_path = temp.join("team.toml");
    let content = r#"schema_version = 2
[opencode_models]
"custom-model" = ["low"]
"#;
    std::fs::write(&toml_path, content).unwrap();
    let out = run(&["config", "get", "opencode_models.deepseek-v4.1-flash", "--file", toml_path.to_str().unwrap()]);
    assert!(out.status.success(), "get deepseek failed: {}", stderr(&out));
    assert!(stdout(&out).contains("max"));

    let out2 = run(&["config", "get", "opencode_models.custom-model", "--file", toml_path.to_str().unwrap()]);
    assert!(out2.status.success(), "get custom failed: {}", stderr(&out2));
    assert!(stdout(&out2).contains("low"));

    let _ = std::fs::remove_dir_all(&temp);
}

/// R2: `on_cost_cap = "kill"` needs a stop key on the kind's harness and on its
/// one-hop fallback's. `worker spawn` refuses such a kind itself (the load path
/// is a bare parse), before any worktree or pane exists; an omp kind with kill
/// still spawns past the guard.
#[test]
fn config_kill_refused_at_spawn() {
    let repo = std::env::temp_dir().join(format!("quaqua-kill-spawn-{}", std::process::id()));
    for (name, needle) in [
        ("kill-on-claude.toml", "harness 'claude'"),
        ("kill-on-pi.toml", "harness 'pi'"),
        ("kill-fallback-claude.toml", "fallback 'rv'"),
    ] {
        let path = fixtures_dir().join("invalid").join(name);
        let out = Command::new(env!("CARGO_BIN_EXE_quaqua"))
            .env("QUAQUA_TEAM_TOML", &path)
            .env("QUAQUA_TEST", "1")
            .env("QUAQUA_LAYOUT_SHADOW", "1")
            .env("QUAQUA_OWNER", "cos")
            .env("QUAQUA_RUNS_DIR", std::env::temp_dir().join("quaqua-kill-runs"))
            .args(["worker", "spawn", "ni-kill", "ni", &repo.to_string_lossy()])
            .output()
            .expect("spawn quaqua");
        let err = stderr(&out);
        assert!(!out.status.success(), "{name}: spawn must be refused");
        assert!(
            err.contains("kind 'ni'") && err.contains("on_cost_cap") && err.contains(needle),
            "{name}: {err:?}"
        );
        assert!(!repo.exists(), "{name}: refusal must precede any worktree work");
    }
}

/// kill on omp with a stop-key fallback stays valid.
#[test]
fn config_kill_on_omp_validates() {
    let body = std::fs::read_to_string(repo_team()).expect("read team.toml");
    let ok = body.replacen(
        "[kinds.tieude]\nharness = \"omp\"\n",
        "[kinds.tieude]\nharness = \"omp\"\non_cost_cap = \"kill\"\n",
        1,
    );
    assert_ne!(ok, body, "fixture edit did not apply");
    let tmp = std::env::temp_dir().join(format!("quaqua-kill-omp-{}.toml", std::process::id()));
    std::fs::write(&tmp, ok).unwrap();
    let out = run(&["config", "validate", "--file", &tmp.to_string_lossy()]);
    let _ = std::fs::remove_file(&tmp);
    assert!(out.status.success(), "kill on omp must load: {}", stderr(&out));
}
