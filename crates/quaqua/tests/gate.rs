//! `quaqua gate` policy tests — every case from `tests/test_herdr_gate.sh`,
//! checked in-process against the ported tokenizer + policy, plus the outward
//! classifier (`git push` / gh writes → ask, force+main → deny), and tests
//! that drive the built binary through the stdin hook-JSON contract.

use quaqua::gate::{check, check_ctx, check_with_pr, PrFacts, PrLookup, Verdict};
use std::cell::RefCell;
use std::io::Write;
use std::process::{Command, Stdio};

#[path = "common/gitfix.rs"]
mod gitfix;

/// A PR lookup stub: never touches the network, records the selector the gate
/// asked for.
struct StubPr {
    result: Result<PrFacts, String>,
    seen: RefCell<Vec<Option<String>>>,
}

impl StubPr {
    fn ok(body: &str, comments: &[&str]) -> Self {
        StubPr {
            result: Ok(PrFacts {
                body: body.to_string(),
                comments: comments.iter().map(|c| c.to_string()).collect(),
            }),
            seen: RefCell::new(Vec::new()),
        }
    }
    fn fail(reason: &str) -> Self {
        StubPr {
            result: Err(reason.to_string()),
            seen: RefCell::new(Vec::new()),
        }
    }
}

impl PrLookup for StubPr {
    fn view(&self, selector: Option<&str>, _cwd: Option<&str>) -> Result<PrFacts, String> {
        self.seen.borrow_mut().push(selector.map(String::from));
        self.result.clone()
    }
}

fn assert_cases(cases: &[&str], want: Verdict, group: &str) {
    for c in cases {
        assert_eq!(check(c), want, "group `{group}`: case {c:?}");
    }
}

#[test]
fn direct_herdr_calls_are_blocked() {
    assert_cases(
        &[
            "herdr pane split --current --direction right",
            "herdr pane move w1:p5 --new-tab --label x",
            "herdr tab create --label x --no-focus",
            "herdr agent start sude-x --kind pi --pane w1:p5",
            "herdr workspace create --label baro --no-focus",
            "herdr workspace close w7",
            "cd ~/quaqua && herdr pane split --current",
            "X=$(herdr tab create --label y)",
        ],
        Verdict::Block,
        "direct",
    );
}

#[test]
fn equivalent_spellings_are_blocked() {
    assert_cases(
        &[
            "\"herdr\" pane split --current",
            "'herdr' pane move w1:p5 --new-tab",
            "h\\e\\rdr tab create --label x",
            "/usr/local/bin/herdr workspace close w7",
            "./herdr tab create --label x",
            "bash -c 'herdr agent start sude-x --kind pi'",
            "sh -c \"herdr workspace create --label b\"",
            "bash -c 'cd /tmp && herdr pane split'",
            "eval 'herdr pane split --current'",
            "sudo herdr pane split --current",
            "sudo -u root herdr workspace close w7",
            "env FOO=1 herdr workspace close w7",
            "FOO=bar herdr tab create",
            "command herdr agent start sude-x",
            "xargs herdr pane split",
            "time herdr pane move w1:p5 --new-tab",
            "if herdr workspace close w7; then echo x; fi",
            "echo `herdr tab create`",
            "y=\"$(herdr agent start z)\"",
            "herdr >&2 pane split",
        ],
        Verdict::Block,
        "spellings",
    );
}

#[test]
fn scripts_and_read_only_herdr_calls_are_allowed() {
    assert_cases(
        &[
            "quaqua worker spawn sude-x sude ~/repo",
            "quaqua grid reconcile --tab x-workers --dry-run",
            "herdr agent list",
            "herdr agent read sude-x --lines 80",
            "herdr pane send-keys w1:p5 enter",
            "herdr tab list",
            "herdr workspace list",
            "herdr workspace get w7",
            "quaqua lead cleanup baro",
            "grep -n \"herdr pane split\" crates/quaqua/src/lib.rs",
            "quaqua worker spawn sude-x sude ~/repo",
            "bash -c 'echo no herdr here'",
            "echo $(herdr agent list)",
        ],
        Verdict::Allow,
        "allowed",
    );
}

#[test]
fn argument_text_and_heredocs_are_allowed() {
    assert_cases(
        &[
            "printf '%s\\n' 'example: herdr pane split --current'",
            "printf \"%s\\n\" \"example: herdr pane split --current\"",
            "echo \"see herdr tab create in docs\"",
            "note='herdr agent start'",
            "cat <<< \"herdr pane split\"",
            "var=$(printf \"herdr agent start\")",
            "herdr agent prompt sude-x \"please run herdr pane split\"",
            "echo hi >&2 && herdr agent list",
            "cat <<EOF\nuse herdr pane split to arrange\nEOF",
            "cat <<-EOF\n\therdr agent start sude-x\n\tEOF",
        ],
        Verdict::Allow,
        "argument-text",
    );
    // a real command beside a heredoc is still blocked
    assert_eq!(
        check("cat <<EOF; herdr pane split\nbody\nEOF"),
        Verdict::Block,
        "heredoc with a real command beside it",
    );
}

#[test]
fn help_positions_are_allowed() {
    assert_cases(
        &[
            "herdr pane split --help",
            "herdr pane split -h",
            "herdr pane move --help",
            "herdr pane move -h",
            "herdr tab create --help",
            "herdr tab create -h",
            "herdr workspace create --help",
            "herdr workspace create -h",
            "herdr workspace close --help",
            "herdr workspace close -h",
            "herdr agent start --help",
            "herdr agent start -h",
            "herdr --help",
            "herdr -h",
            "herdr --help pane split",
            "herdr -h tab create",
            "herdr --session quaqua pane split --help",
            "herdr --session quaqua workspace close -h",
            "herdr pane split --help --current",
            "bash -c 'herdr agent start --help'",
            "sudo herdr tab create -h",
            "X=$(herdr pane move -h)",
        ],
        Verdict::Allow,
        "help-position",
    );
}

#[test]
fn help_shaped_words_outside_a_help_position_are_blocked() {
    assert_cases(
        &[
            "herdr pane split --pane --help",
            "herdr pane split --current --help",
            "herdr pane split nosuchpane -h",
            "herdr tab create --label --help",
            "herdr tab create --workspace x -h",
            "herdr workspace close --group --help",
            "herdr agent start --kind --help --pane x",
            "herdr agent start sude-x --kind pi --pane w1:p5 --help",
            "herdr pane split --hel",
            "herdr pane split --helpful",
            "herdr pane split -H",
            "herdr pane split -help",
            "herdr pane split -hx",
            "herdr pane split x -h",
        ],
        Verdict::Block,
        "help-shaped",
    );
}

#[test]
fn empty_substitution_words_still_resolve_the_executable() {
    // Finding 1 (runs/suhuynh-r5r1.md): bash joins words with TAB and re-splits
    // with IFS, so an empty word from an unquoted command substitution vanishes;
    // the executable is still `herdr` and the call is blocked.
    assert_cases(
        &[
            "$(true) herdr pane split",
            "`:` herdr tab create --label x",
            "sudo $(true) herdr workspace close w7",
            "X=1 $(true) herdr agent start x",
        ],
        Verdict::Block,
        "empty-word",
    );
}

#[test]
fn outward_pushes_and_gh_writes_ask() {
    for c in [
        "git push",
        "git push origin",
        "git push origin feature",
        "git push origin HEAD:feature",
        "git push origin refs/heads/topic:refs/heads/topic",
        "git push -u origin feature",
        "git push --tags origin",
        "git push -o ci.skip origin feature",
        "git push --repo origin origin feature",
        "git -C ~/repo push origin feature",
        "git -c push.default=current push origin feature",
        "git --no-pager push origin feature",
        "sudo git push origin feature",
        "env GIT_SSH_COMMAND=ssh git push origin feature",
        "cd ~/repo && git push origin feature",
        "bash -c 'git push origin feature'",
        "echo started; git push origin feature",
        "git push origin feature | cat",
        "gh pr create --title t --body b",
        "gh pr edit 42 --title t",
        "gh pr close 42",
        "gh pr comment 42 --body hi",
        "gh pr ready 42",
        "gh pr merge 42 --squash",
        "gh pr merge --auto 42",
        "gh issue create --title t",
        "gh issue edit 3 --title t",
        "gh issue close 3",
        "gh issue comment 3 --body hi",
        "gh release create v1.0",
        "gh release upload v1.0 dist/x.zip",
        "gh repo create --private",
        "gh repo delete --yes",
        "gh repo sync x/y",
        "gh -R x/y pr comment 42 --body hi",
        "cd /tmp && gh repo transfer x/y z",
    ] {
        assert!(
            matches!(check(c), Verdict::Ask(_)),
            "want ask, got {:?}: {c:?}",
            check(c)
        );
    }
    let Verdict::Ask(r) = check("git push origin feature") else {
        panic!("want ask");
    };
    assert!(r.contains("AGENTS.md rule 3"), "{r}");
}

#[test]
fn force_pushes_and_main_are_denied() {
    for c in [
        "git push -f origin feature",
        "git push --force",
        "git push --force-with-lease origin feature",
        "git push --force-with-lease=main origin feature",
        "git push --force-if-includes origin feature",
        "git push origin +feature",
        "git push origin +main",
        "git push origin main",
        "git push origin master",
        "git push origin HEAD:main",
        "git push origin feature:main",
        "git push origin refs/heads/feature:refs/heads/master",
        "git push origin :main",
        "git push --delete origin main",
        "git push --all origin",
        "git push --mirror",
        "git push -uf origin main",
        "cd ~/repo && git push --force-with-lease",
        "sudo git push origin main",
        "git -C /repo push origin main",
        "git -c push.default=current push origin main",
        "gh pr merge main",
        "gh pr merge master --squash",
        "gh pr merge refs/heads/main",
    ] {
        assert!(
            matches!(check(c), Verdict::Deny(_)),
            "want deny, got {:?}: {c:?}",
            check(c)
        );
    }
    let Verdict::Deny(r) = check("git push -f") else {
        panic!("want deny");
    };
    assert!(r.contains("force push"), "{r}");
    let Verdict::Deny(r) = check("git push origin main") else {
        panic!("want deny");
    };
    assert!(r.contains("main"), "{r}");
}

#[test]
fn read_only_git_and_gh_stay_allowed() {
    for c in [
        "git status",
        "git status --short",
        "git log --oneline -5",
        "git fetch origin",
        "git diff HEAD~1",
        "git show --stat HEAD",
        "git branch --show-current",
        "git rev-parse HEAD",
        "git worktree list",
        "git remote -v",
        "git push --help",
        "git push -h",
        "git --help",
        "gh pr view 42",
        "gh pr list",
        "gh pr status",
        "gh pr diff 42",
        "gh pr checks 42",
        "gh issue view 3",
        "gh issue list",
        "gh release list",
        "gh release view v1",
        "gh release download v1",
        "gh repo view x/y",
        "gh repo list",
        "gh repo clone x/y",
        "gh run list",
        "gh run view 12",
        "gh api repos/x/y",
        "gh auth status",
        "gh pr create --help",
        "gh pr -h",
        "gh -h",
        "git -C /repo status",
        "sudo git fetch origin",
        "cd /repo && git fetch",
        // outward spellings inside argument text or a heredoc body stay quiet
        "printf '%s\\n' 'git push origin main'",
        "echo \"run git push to deploy\"",
        "grep -rn \"git push\" README.md",
        "cat <<EOF\ngit push --force origin main\nEOF",
        "cat <<EOF; git status\nbody\nEOF",
    ] {
        assert_eq!(check(c), Verdict::Allow, "case {c:?}");
    }
}

#[test]
fn outward_decisions_compose_over_compounds() {
    // a prompt inside a compound command still fires
    for c in [
        "cd ~/repo && gh issue comment 3 --body y",
        "git push origin feature; ls",
        "mkdir -p out && gh release create v2",
    ] {
        assert!(matches!(check(c), Verdict::Ask(_)), "want ask: {c:?}");
    }
    // deny outranks ask; the herdr block outranks ask too (an approval must
    // not let a pane/workspace spawn through as a side effect)
    assert!(matches!(
        check("git push origin main && gh pr create"),
        Verdict::Deny(_)
    ));
    assert!(matches!(
        check("herdr pane split && git push origin feature"),
        Verdict::Block
    ));
    assert!(matches!(
        check("herdr pane split && gh pr create"),
        Verdict::Block
    ));
    assert!(matches!(
        check("git push -f && herdr pane split"),
        Verdict::Deny(_)
    ));
}

#[test]
fn gh_pr_merge_asks_when_the_pr_says_wip_or_cancelled_or_has_unchecked_items() {
    let cases = [
        ("WIP: half done", &[][..], "WIP"),
        ("ready", &["cancelled mid-run"][..], "cancelled mid-run"),
        ("- [x] done\n- [ ] still open", &[][..], "unchecked"),
    ];
    for (body, comments, needle) in cases {
        let stub = StubPr::ok(body, comments);
        match check_with_pr("gh pr merge 42 --squash", None, &stub) {
            Verdict::Ask(reason) => {
                assert!(
                    reason.contains(needle),
                    "reason {reason:?} lacks {needle:?}"
                );
                assert!(reason.contains("42"), "reason must name the PR: {reason:?}");
            }
            other => panic!("want ask, got {other:?} for {body}"),
        }
        assert_eq!(stub.seen.borrow().as_slice(), &[Some("42".to_string())]);
    }
}

#[test]
fn gh_pr_merge_lookup_failure_asks_never_allows() {
    let stub = StubPr::fail("gh: not logged in");
    match check_with_pr("gh pr merge 42", None, &stub) {
        Verdict::Ask(reason) => {
            assert!(reason.contains("could not verify"), "{reason}");
            assert!(reason.contains("not logged in"), "{reason}");
        }
        other => panic!("want ask on a failed lookup, got {other:?}"),
    }
}

#[test]
fn a_clean_pr_still_asks_and_merging_to_main_still_denies_without_a_lookup() {
    let clean = StubPr::ok("all good", &[]);
    let verdict = check_with_pr("gh pr merge --auto 42", None, &clean);
    assert!(matches!(verdict, Verdict::Ask(_)), "{verdict:?}");
    assert_eq!(clean.seen.borrow().as_slice(), &[Some("42".to_string())]);

    // main is denied before any lookup; the stub must not be consulted.
    let main = StubPr::fail("must not be called");
    assert!(matches!(
        check_with_pr("gh pr merge main --squash", None, &main),
        Verdict::Deny(_)
    ));
    assert!(main.seen.borrow().is_empty());
}

#[test]
fn pkill_f_and_full_are_denied() {
    let cases = [
        "pkill -f quaqua",
        "pkill --full quaqua",
        "pkill -9 -f quaqua",
        "pkill -9f quaqua",
        "pkill -f9 quaqua",
        "sudo pkill -f quaqua",
        "bash -c 'pkill -f quaqua'",
    ];
    for c in cases {
        match check(c) {
            Verdict::Deny(reason) => {
                assert!(
                    reason.contains("use `pgrep -f` then kill the PID"),
                    "case {c}: unexpected reason {reason}"
                );
            }
            other => panic!("case {c}: expected Deny, got {other:?}"),
        }
    }
}

#[test]
fn pkill_without_f_and_pgrep_stay_allowed() {
    let cases = [
        "pkill quaqua",
        "pkill -9 quaqua",
        "pgrep -f quaqua",
        "pgrep --full quaqua",
        "kill 1234",
    ];
    for c in cases {
        assert_eq!(check(c), Verdict::Allow, "case {c}");
    }
}

#[test]
fn removed_worker_scripts_are_denied_with_the_replacement_hint() {
    let cases = [
        (
            "scripts/worker/prompt-worker.sh sude-x",
            "quaqua worker prompt",
        ),
        ("./scripts/worker/spawn-worker.sh x", "quaqua worker spawn"),
        (
            "bash scripts/worker/cleanup-worker.sh sude-x ~/repo",
            "quaqua worker cleanup",
        ),
        (
            "sudo ~/quaqua/scripts/worker/grid-workers.sh --tab x",
            "quaqua grid reconcile",
        ),
        ("env FOO=1 scripts/worker/worker-cap.sh cos", "quaqua cap"),
    ];
    for (cmd, hint) in cases {
        match check(cmd) {
            Verdict::Deny(reason) => {
                assert!(reason.contains(hint), "reason {reason:?} lacks {hint:?}");
                assert!(reason.contains("removed"), "{reason}");
            }
            other => panic!("want deny, got {other:?}: {cmd:?}"),
        }
    }
    // The path as argument text is not a call.
    assert_eq!(
        check("grep -rn \"scripts/worker/prompt-worker.sh\" docs/"),
        Verdict::Allow
    );
    assert_eq!(check("echo scripts/worker/spawn-worker.sh"), Verdict::Allow);
}

#[test]
fn gate_binary_asks_with_the_wip_reason_through_a_stub_gh() {
    let dir = std::env::temp_dir().join(format!("quaqua-gate-gh-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp dir");
    let gh = dir.join("gh");
    std::fs::write(
        &gh,
        "#!/bin/sh\nprintf '%s\\n' '{\"body\":\"WIP: do not merge\",\"comments\":[]}'\n",
    )
    .expect("write gh");
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o755)).expect("chmod");

    let bin = env!("CARGO_BIN_EXE_quaqua");
    let mut child = Command::new(bin)
        .arg("gate")
        .env("QUAQUA_GH_BIN", &gh)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn quaqua");
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(br#"{"tool_name":"Bash","tool_input":{"command":"gh pr merge 42 --squash"}}"#)
        .expect("write");
    let out = child.wait_with_output().expect("wait");
    assert_eq!(out.status.code(), Some(0));
    let decision: serde_json::Value = serde_json::from_slice(&out.stdout).expect("decision JSON");
    assert_eq!(decision["hookSpecificOutput"]["permissionDecision"], "ask");
    let reason = decision["hookSpecificOutput"]["permissionDecisionReason"]
        .as_str()
        .expect("reason");
    assert!(reason.contains("WIP"), "{reason}");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn bare_push_resolves_the_checked_out_branch() {
    let dir = std::env::temp_dir().join(format!("quaqua-gate-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp dir");
    let git = |args: &[&str]| {
        let out = gitfix::cmd("git")
            .arg("-C")
            .arg(&dir)
            .args(args)
            .output()
            .expect("run git");
        assert!(out.status.success(), "git {args:?}: {out:?}");
    };
    git(&["init", "-q"]);

    // no refspec in the command text: the branch the push would send decides
    git(&["symbolic-ref", "HEAD", "refs/heads/main"]);
    let cwd = dir.to_str().expect("utf8 temp dir");
    assert!(matches!(check_ctx("git push", Some(cwd)), Verdict::Deny(_)));
    assert!(matches!(
        check_ctx("git push origin", Some(cwd)),
        Verdict::Deny(_)
    ));
    assert!(matches!(
        check_ctx("git push -u origin", Some(cwd)),
        Verdict::Deny(_)
    ));
    git(&["symbolic-ref", "HEAD", "refs/heads/feature"]);
    assert!(matches!(check_ctx("git push", Some(cwd)), Verdict::Ask(_)));
    assert!(matches!(
        check_ctx("git push origin", Some(cwd)),
        Verdict::Ask(_)
    ));
    // `git -C <dir>` moves the lookup to that repository
    assert!(matches!(
        check_ctx(&format!("git -C {} push", dir.display()), Some("/")),
        Verdict::Ask(_)
    ));
    // no cwd (hook without cwd): the branch is unknown, so the push asks
    assert!(matches!(check_ctx("git push", None), Verdict::Ask(_)));

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn binary_emits_pretooluse_json_for_outward_actions() {
    let bin = env!("CARGO_BIN_EXE_quaqua");
    let run = |json: &str| -> (Option<i32>, String) {
        let mut child = Command::new(bin)
            .arg("gate")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("failed to spawn the quaqua binary");
        child
            .stdin
            .take()
            .expect("stdin")
            .write_all(json.as_bytes())
            .expect("write hook json");
        let out = child.wait_with_output().expect("wait");
        (
            out.status.code(),
            String::from_utf8_lossy(&out.stdout).to_string(),
        )
    };
    let decision = |stdout: &str| -> serde_json::Value {
        serde_json::from_str(stdout.trim())
            .unwrap_or_else(|e| panic!("stdout is not JSON ({e}): {stdout:?}"))
    };

    let (code, stdout) =
        run(r#"{"tool_name":"Bash","tool_input":{"command":"git push origin feature"}}"#);
    assert_eq!(code, Some(0), "ask must exit 0");
    let v = decision(&stdout);
    assert_eq!(v["hookSpecificOutput"]["hookEventName"], "PreToolUse");
    assert_eq!(v["hookSpecificOutput"]["permissionDecision"], "ask");
    assert!(v["hookSpecificOutput"]["permissionDecisionReason"]
        .as_str()
        .expect("reason")
        .contains("AGENTS.md rule 3"));

    let (code, stdout) = run(
        r#"{"tool_name":"Bash","tool_input":{"command":"gh pr create --title x"},"cwd":"/tmp"}"#,
    );
    assert_eq!(code, Some(0), "ask must exit 0");
    assert_eq!(
        decision(&stdout)["hookSpecificOutput"]["permissionDecision"],
        "ask"
    );

    let (code, stdout) =
        run(r#"{"tool_name":"Bash","tool_input":{"command":"git push --force origin"}}"#);
    assert_eq!(code, Some(0), "deny must exit 0");
    assert_eq!(
        decision(&stdout)["hookSpecificOutput"]["permissionDecision"],
        "deny"
    );

    // read-only commands keep the silent exit 0
    let (code, stdout) = run(r#"{"tool_name":"Bash","tool_input":{"command":"git status"}}"#);
    assert_eq!(code, Some(0));
    assert!(stdout.trim().is_empty(), "no decision output: {stdout:?}");
}

#[test]
fn binary_follows_the_hook_json_contract() {
    let bin = env!("CARGO_BIN_EXE_quaqua");
    let run = |json: &str| -> Option<i32> {
        let mut child = Command::new(bin)
            .arg("gate")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("failed to spawn the quaqua binary");
        child
            .stdin
            .take()
            .expect("stdin")
            .write_all(json.as_bytes())
            .expect("write hook json");
        child.wait().expect("wait").code()
    };
    assert_eq!(
        run(r#"{"tool_name":"Bash","tool_input":{"command":"herdr pane split"}}"#),
        Some(2),
        "blocked command must exit 2",
    );
    assert_eq!(
        run(r#"{"tool_name":"Bash","tool_input":{"command":"herdr agent list"}}"#),
        Some(0),
        "allowed command must exit 0",
    );
    assert_eq!(run(""), Some(0), "empty stdin must exit 0");
    assert_eq!(run("   \n"), Some(0), "whitespace-only stdin must exit 0");
    assert_eq!(
        run("not json"),
        Some(2),
        "unparseable non-empty stdin must fail closed (finding 2)",
    );
}

/// Stale-session guard (issue #72): SessionStart records the rule baseline; a
/// later `quaqua worker` call warns once after a rule file changed, never blocks.
#[test]
fn binary_warns_on_rule_drift_after_session_start() {
    let bin = env!("CARGO_BIN_EXE_quaqua");
    let ws = std::env::temp_dir().join(format!("quaqua-gate-drift-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&ws);
    std::fs::create_dir_all(ws.join("rules/_all")).unwrap();
    std::fs::write(ws.join("AGENTS.md"), "v1").unwrap();
    std::fs::write(ws.join("rules/_all/a.md"), "v1").unwrap();
    let run = |args: &[&str], json: &str| -> (Option<i32>, String) {
        let mut child = Command::new(bin)
            .args(args)
            .env("QUAQUA_HOME", &ws)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("failed to spawn the quaqua binary");
        child
            .stdin
            .take()
            .expect("stdin")
            .write_all(json.as_bytes())
            .expect("write");
        let out = child.wait_with_output().expect("wait");
        (
            out.status.code(),
            String::from_utf8_lossy(&out.stdout).into_owned(),
        )
    };
    let spawn = r#"{"session_id":"sess-1","tool_name":"Bash","tool_input":{"command":"quaqua worker spawn sude-x sude /r"}}"#;

    let (code, out) = run(
        &["gate", "--session-start"],
        r#"{"session_id":"sess-1","source":"startup"}"#,
    );
    assert_eq!((code, out.as_str()), (Some(0), ""));
    let (code, out) = run(&["gate"], spawn);
    assert_eq!((code, out.trim()), (Some(0), ""), "no drift, no output");

    std::fs::write(ws.join("rules/_all/a.md"), "v2").unwrap();
    let (code, out) = run(
        &["gate"],
        r#"{"session_id":"sess-1","tool_name":"Bash","tool_input":{"command":"quaqua status"}}"#,
    );
    assert_eq!(
        (code, out.trim()),
        (Some(0), ""),
        "non-dispatch commands stay quiet"
    );
    let (code, out) = run(&["gate"], spawn);
    assert_eq!(code, Some(0), "drift never blocks");
    let v: serde_json::Value = serde_json::from_str(out.trim()).expect("hook JSON");
    let ctx = v["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap();
    assert!(ctx.contains("rules/_all/a.md"), "{ctx}");
    assert_eq!(v["hookSpecificOutput"]["hookEventName"], "PreToolUse");
    let (_, out) = run(&["gate"], spawn);
    assert_eq!(out.trim(), "", "warns once per change");

    let (code, out) = run(
        &["gate", "--session-start"],
        r#"{"session_id":"sess-1","source":"compact"}"#,
    );
    assert_eq!(code, Some(0));
    assert!(
        out.contains("Re-read") && out.contains("AGENTS.md"),
        "{out}"
    );
}

use quaqua::herdr::HerdrOps;
use std::collections::HashMap;

static TEST_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

struct EnvGuard {
    _lock: std::sync::MutexGuard<'static, ()>,
    vars: Vec<(&'static str, Option<String>)>,
}

impl EnvGuard {
    fn set(pairs: &[(&'static str, &str)]) -> Self {
        let lock = TEST_ENV_LOCK.lock().unwrap();
        let mut vars = Vec::new();
        for &(k, v) in pairs {
            vars.push((k, std::env::var(k).ok()));
            std::env::set_var(k, v);
        }
        EnvGuard { _lock: lock, vars }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        for (k, v) in &self.vars {
            match v {
                Some(prev) => std::env::set_var(k, prev),
                None => std::env::remove_var(k),
            }
        }
    }
}

type SessionPane = (Option<String>, Option<String>);

struct FakeSessionStartHerdr {
    current_pane: Option<String>,
    panes: RefCell<HashMap<String, SessionPane>>,
    calls: RefCell<Vec<Vec<String>>>,
    fail_on: Option<String>,
}

impl FakeSessionStartHerdr {
    fn new(current_pane: Option<&str>) -> Self {
        Self {
            current_pane: current_pane.map(String::from),
            panes: RefCell::new(HashMap::new()),
            calls: RefCell::new(Vec::new()),
            fail_on: None,
        }
    }

    fn with_pane(self, pane_id: &str, role: Option<&str>, session: Option<&str>) -> Self {
        self.panes.borrow_mut().insert(
            pane_id.to_string(),
            (role.map(String::from), session.map(String::from)),
        );
        self
    }

    fn with_failure(mut self, fail_subcmd: &str) -> Self {
        self.fail_on = Some(fail_subcmd.to_string());
        self
    }
}

impl HerdrOps for FakeSessionStartHerdr {
    fn run(&self, args: &[&str]) -> std::io::Result<std::process::Output> {
        self.calls
            .borrow_mut()
            .push(args.iter().map(|s| s.to_string()).collect());

        if let Some(sub) = &self.fail_on {
            if args.iter().any(|a| a == sub) {
                return Ok(std::process::Output {
                    status: std::os::unix::process::ExitStatusExt::from_raw(256),
                    stdout: vec![],
                    stderr: b"simulated herdr failure".to_vec(),
                });
            }
        }

        if args.len() >= 2 && args[0] == "pane" && args[1] == "current" {
            if let Some(p) = &self.current_pane {
                let json = format!(r#"{{"result":{{"pane":{{"pane_id":"{p}"}}}}}}"#);
                return Ok(std::process::Output {
                    status: std::os::unix::process::ExitStatusExt::from_raw(0),
                    stdout: json.into_bytes(),
                    stderr: vec![],
                });
            } else {
                return Ok(std::process::Output {
                    status: std::os::unix::process::ExitStatusExt::from_raw(256),
                    stdout: vec![],
                    stderr: b"no current pane".to_vec(),
                });
            }
        }

        if args.len() >= 3 && args[0] == "pane" && args[1] == "get" {
            let pane_id = args[2];
            let panes = self.panes.borrow();
            if let Some((role_opt, sess_opt)) = panes.get(pane_id) {
                let mut tokens_obj = serde_json::Map::new();
                if let Some(r) = role_opt {
                    tokens_obj.insert(
                        "grid_role".to_string(),
                        serde_json::Value::String(r.clone()),
                    );
                }
                let mut pane_obj = serde_json::Map::new();
                pane_obj.insert(
                    "pane_id".to_string(),
                    serde_json::Value::String(pane_id.to_string()),
                );
                pane_obj.insert("tokens".to_string(), serde_json::Value::Object(tokens_obj));
                if let Some(s) = sess_opt {
                    pane_obj.insert(
                        "agent_session".to_string(),
                        serde_json::json!({ "value": s }),
                    );
                }
                let val = serde_json::json!({
                    "result": {
                        "pane": pane_obj
                    }
                });
                return Ok(std::process::Output {
                    status: std::os::unix::process::ExitStatusExt::from_raw(0),
                    stdout: serde_json::to_vec(&val).unwrap(),
                    stderr: vec![],
                });
            } else {
                return Ok(std::process::Output {
                    status: std::os::unix::process::ExitStatusExt::from_raw(256),
                    stdout: vec![],
                    stderr: b"pane not found".to_vec(),
                });
            }
        }

        if args.len() >= 2
            && args[0] == "pane"
            && (args[1] == "report-metadata" || args[1] == "rename")
        {
            return Ok(std::process::Output {
                status: std::os::unix::process::ExitStatusExt::from_raw(0),
                stdout: b"{\"result\":{\"ok\":true}}".to_vec(),
                stderr: vec![],
            });
        }

        Ok(std::process::Output {
            status: std::os::unix::process::ExitStatusExt::from_raw(0),
            stdout: b"{\"result\":{\"ok\":true}}".to_vec(),
            stderr: vec![],
        })
    }

    fn agent_list(&self) -> std::io::Result<String> {
        unimplemented!()
    }
    fn agent_get(&self, _: &str) -> std::io::Result<(bool, String)> {
        unimplemented!()
    }
    fn agent_read(&self, _: &str, _: &str, _: u32) -> std::io::Result<String> {
        unimplemented!()
    }
    fn agent_start(
        &self,
        _: &str,
        _: &str,
        _: &str,
        _: u32,
        _: &[&str],
    ) -> std::io::Result<std::process::Output> {
        unimplemented!()
    }
    fn pane_split(
        &self,
        _: &str,
        _: &str,
        _: &[(&str, &str)],
    ) -> std::io::Result<std::process::Output> {
        unimplemented!()
    }
    fn pane_close(&self, _: &str) -> std::io::Result<std::process::Output> {
        unimplemented!()
    }
    fn pane_report_metadata(&self, _: &str, _: &[&str]) -> std::io::Result<std::process::Output> {
        unimplemented!()
    }
    fn pane_wait_output(&self, _: &str, _: &str, _: u32) -> bool {
        unimplemented!()
    }
    fn pane_read(&self, _: &str, _: &str, _: u32) -> std::io::Result<String> {
        unimplemented!()
    }
    fn pane_send_keys(&self, _: &str, _: &str) -> std::io::Result<std::process::Output> {
        unimplemented!()
    }
    fn agent_send_keys(&self, _: &str, _: &str) -> std::io::Result<std::process::Output> {
        unimplemented!()
    }
    fn agent_prompt_nowait(&self, _: &str, _: &str) -> std::io::Result<std::process::Output> {
        unimplemented!()
    }
    fn agent_prompt_wait_background(
        &self,
        _: &str,
        _: &str,
        _: u32,
        _: std::fs::File,
    ) -> std::io::Result<std::process::Child> {
        unimplemented!()
    }
}

#[test]
fn session_start_tags_cos_pane_in_workspace() {
    let ws = std::env::temp_dir().join(format!("quaqua-cos-tag-ws-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&ws);
    std::fs::create_dir_all(ws.join("projects")).unwrap();
    std::fs::create_dir_all(ws.join("rules/_all")).unwrap();
    std::fs::write(ws.join("projects/INDEX.md"), "# Projects\n").unwrap();
    std::fs::write(ws.join("AGENTS.md"), "CoS instructions").unwrap();

    let fake = FakeSessionStartHerdr::new(Some("w1:pCos")).with_pane(
        "w1:pCos",
        None,
        Some("sess-cos-123"),
    );

    let _env = EnvGuard::set(&[("HERDR_PANE", "w1:pCos")]);

    let code = quaqua::gate::run_session_start_with(
        &fake,
        r#"{"session_id":"s1","source":"startup"}"#,
        &ws,
        Some(&ws),
    );
    assert_eq!(code, 0);

    let calls = fake.calls.borrow();
    let has_metadata = calls.iter().any(|c| {
        c.len() >= 4
            && c[0] == "pane"
            && c[1] == "report-metadata"
            && c[2] == "w1:pCos"
            && c.contains(&"grid_role=cos".to_string())
    });
    assert!(
        has_metadata,
        "must call pane report-metadata with grid_role=cos: {calls:?}"
    );

    let has_rename = calls.iter().any(|c| {
        c.len() >= 4 && c[0] == "pane" && c[1] == "rename" && c[2] == "w1:pCos" && c[3] == "🍑 CoS"
    });
    assert!(has_rename, "must rename pane to 🍑 CoS: {calls:?}");

    // Already-cos is an idempotent no-op or harmless retag
    let fake_already_cos = FakeSessionStartHerdr::new(Some("w1:pCos")).with_pane(
        "w1:pCos",
        Some("cos"),
        Some("sess-cos-123"),
    );
    let code = quaqua::gate::run_session_start_with(
        &fake_already_cos,
        r#"{"session_id":"s2","source":"startup"}"#,
        &ws,
        Some(&ws),
    );
    assert_eq!(code, 0);
    let calls2 = fake_already_cos.calls.borrow();
    let has_metadata2 = calls2.iter().any(|c| {
        c.len() >= 4
            && c[0] == "pane"
            && c[1] == "report-metadata"
            && c[2] == "w1:pCos"
            && c.contains(&"grid_role=cos".to_string())
    });
    assert!(
        has_metadata2,
        "must retag already-cos pane idempotently: {calls2:?}"
    );
    let _ = std::fs::remove_dir_all(&ws);
}

#[test]
fn session_start_skips_worker_and_lead_pane() {
    let ws = std::env::temp_dir().join(format!("quaqua-cos-worker-ws-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&ws);
    std::fs::create_dir_all(ws.join("projects")).unwrap();
    std::fs::create_dir_all(ws.join("rules/_all")).unwrap();
    std::fs::write(ws.join("projects/INDEX.md"), "# Projects\n").unwrap();
    std::fs::write(ws.join("AGENTS.md"), "CoS instructions").unwrap();

    // Worker pane
    {
        let fake = FakeSessionStartHerdr::new(Some("w1:pWorker")).with_pane(
            "w1:pWorker",
            Some("worker"),
            Some("sess-worker-1"),
        );

        let _env = EnvGuard::set(&[("HERDR_PANE", "w1:pWorker")]);

        let code = quaqua::gate::run_session_start_with(
            &fake,
            r#"{"session_id":"s1","source":"startup"}"#,
            &ws,
            Some(&ws),
        );
        assert_eq!(code, 0);

        let calls = fake.calls.borrow();
        let has_metadata = calls
            .iter()
            .any(|c| c.get(1).map(|s| s.as_str()) == Some("report-metadata"));
        assert!(!has_metadata, "must not tag worker pane: {calls:?}");
    }

    // Orchestrator / Lead pane
    {
        let fake = FakeSessionStartHerdr::new(Some("w1:pLead")).with_pane(
            "w1:pLead",
            Some("orchestrator"),
            Some("sess-lead-1"),
        );

        let _env = EnvGuard::set(&[("HERDR_PANE", "w1:pLead")]);

        let code = quaqua::gate::run_session_start_with(
            &fake,
            r#"{"session_id":"s2","source":"startup"}"#,
            &ws,
            Some(&ws),
        );
        assert_eq!(code, 0);

        let calls = fake.calls.borrow();
        let has_metadata = calls
            .iter()
            .any(|c| c.get(1).map(|s| s.as_str()) == Some("report-metadata"));
        assert!(
            !has_metadata,
            "must not tag lead/orchestrator pane: {calls:?}"
        );
    }

    let _ = std::fs::remove_dir_all(&ws);
}

#[test]
fn session_start_skips_outside_workspace_and_in_worktree() {
    let ws = std::env::temp_dir().join(format!("quaqua-cos-outside-ws-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&ws);
    std::fs::create_dir_all(ws.join("projects")).unwrap();
    std::fs::create_dir_all(ws.join("rules/_all")).unwrap();
    std::fs::write(ws.join("projects/INDEX.md"), "# Projects\n").unwrap();
    std::fs::write(ws.join("AGENTS.md"), "CoS instructions").unwrap();

    // Case A: cwd is in a worker worktree (.wt/sude-test)
    let wt = ws.join(".wt/sude-test");
    std::fs::create_dir_all(&wt).unwrap();
    {
        let fake =
            FakeSessionStartHerdr::new(Some("w1:p1")).with_pane("w1:p1", None, Some("sess-1"));
        let _env = EnvGuard::set(&[("HERDR_PANE", "w1:p1")]);

        let code = quaqua::gate::run_session_start_with(
            &fake,
            r#"{"session_id":"s1","source":"startup"}"#,
            &ws,
            Some(&wt),
        );
        assert_eq!(code, 0);
        let calls = fake.calls.borrow();
        let has_metadata = calls
            .iter()
            .any(|c| c.get(1).map(|s| s.as_str()) == Some("report-metadata"));
        assert!(!has_metadata, "must not tag in worker worktree: {calls:?}");
    }

    // Case B: cwd is outside workspace altogether
    let outside = std::env::temp_dir().join(format!("quaqua-cos-other-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&outside);
    std::fs::create_dir_all(&outside).unwrap();
    {
        let fake =
            FakeSessionStartHerdr::new(Some("w1:p1")).with_pane("w1:p1", None, Some("sess-1"));
        let _env = EnvGuard::set(&[("HERDR_PANE", "w1:p1")]);

        let code = quaqua::gate::run_session_start_with(
            &fake,
            r#"{"session_id":"s1","source":"startup"}"#,
            &ws,
            Some(&outside),
        );
        assert_eq!(code, 0);
        let calls = fake.calls.borrow();
        let has_metadata = calls
            .iter()
            .any(|c| c.get(1).map(|s| s.as_str()) == Some("report-metadata"));
        assert!(!has_metadata, "must not tag outside workspace: {calls:?}");
    }

    let _ = std::fs::remove_dir_all(&ws);
    let _ = std::fs::remove_dir_all(&outside);
}

#[test]
fn session_start_herdr_error_still_exits_zero() {
    let ws = std::env::temp_dir().join(format!("quaqua-cos-err-ws-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&ws);
    std::fs::create_dir_all(ws.join("projects")).unwrap();
    std::fs::create_dir_all(ws.join("rules/_all")).unwrap();
    std::fs::write(ws.join("projects/INDEX.md"), "# Projects\n").unwrap();
    std::fs::write(ws.join("AGENTS.md"), "CoS instructions").unwrap();

    let fake = FakeSessionStartHerdr::new(Some("w1:pCos"))
        .with_pane("w1:pCos", None, Some("sess-cos-1"))
        .with_failure("report-metadata");

    let _env = EnvGuard::set(&[("HERDR_PANE", "w1:pCos")]);

    let code = quaqua::gate::run_session_start_with(
        &fake,
        r#"{"session_id":"s1","source":"startup"}"#,
        &ws,
        Some(&ws),
    );
    assert_eq!(
        code, 0,
        "must still exit 0 even if herdr report-metadata fails"
    );
    let _ = std::fs::remove_dir_all(&ws);
}

#[test]
fn session_start_tags_cos_pane_outside_quaqua_workspace_with_own_display_name() {
    let ws = std::env::temp_dir().join(format!("quaqua-cos-ws-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&ws);
    std::fs::create_dir_all(ws.join("projects")).unwrap();
    std::fs::create_dir_all(ws.join("rules/_all")).unwrap();
    std::fs::write(ws.join("projects/INDEX.md"), "# Projects\n").unwrap();
    std::fs::write(ws.join("AGENTS.md"), "CoS instructions").unwrap();

    // A second workspace "ttdvkh" outside ws
    let ttdvkh = std::env::temp_dir().join(format!("ttdvkh-cos-ws-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&ttdvkh);
    std::fs::create_dir_all(ttdvkh.join(".cos/runs")).unwrap();
    std::fs::create_dir_all(ttdvkh.join(".cos/projects")).unwrap();
    std::fs::write(
        ttdvkh.join(".cos/identity.md"),
        "# CoS — TTDVKH Chief of Staff\n\nYou are fen's Chief of Staff.\n",
    )
    .unwrap();
    std::fs::write(ttdvkh.join(".cos/projects/INDEX.md"), "# Projects\n").unwrap();

    let fake =
        FakeSessionStartHerdr::new(Some("w7:p2")).with_pane("w7:p2", None, Some("sess-ttdvkh-123"));

    let _env = EnvGuard::set(&[("HERDR_PANE", "w7:p2")]);

    let code = quaqua::gate::run_session_start_with(
        &fake,
        r#"{"session_id":"s-ttdvkh","source":"startup"}"#,
        &ws,
        Some(&ttdvkh),
    );
    assert_eq!(code, 0);

    let calls = fake.calls.borrow();
    let has_metadata = calls.iter().any(|c| {
        c.len() >= 4
            && c[0] == "pane"
            && c[1] == "report-metadata"
            && c[2] == "w7:p2"
            && c.contains(&"grid_role=cos".to_string())
    });
    assert!(
        has_metadata,
        "must call pane report-metadata with grid_role=cos for workspace outside quaqua: {calls:?}"
    );

    let has_display_agent = calls.iter().any(|c| {
        c.iter()
            .position(|a| a == "--display-agent")
            .and_then(|idx| c.get(idx + 1))
            .map(|s| s.contains("CoS") && s.contains("TTDVKH"))
            .unwrap_or(false)
    });
    assert!(
        has_display_agent,
        "must set display-agent containing CoS and TTDVKH: {calls:?}"
    );

    let has_rename = calls.iter().any(|c| {
        c.len() >= 4
            && c[0] == "pane"
            && c[1] == "rename"
            && c[2] == "w7:p2"
            && c[3].contains("TTDVKH")
    });
    assert!(
        has_rename,
        "must rename pane to display name with TTDVKH: {calls:?}"
    );

    assert!(
        ttdvkh.join(".cos/runs/cos.identity.json").is_file(),
        "must record cos.identity.json in ttdvkh/.cos/runs"
    );
    assert!(
        !ws.join("runs/cos.identity.json").is_file(),
        "must not record cos.identity.json in quaqua ws runs"
    );

    let _ = std::fs::remove_dir_all(&ws);
    let _ = std::fs::remove_dir_all(&ttdvkh);
}
