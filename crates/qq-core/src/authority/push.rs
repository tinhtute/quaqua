//! The `push` verb: the one door for `git push` (R11). The handler asks
//! [`check_orchestration`](super::check_orchestration) first, so a Peer or an
//! unfenced driver is refused and journaled by the daemon before git runs.
//! The fence comes from the caller's kind -> harness -> driver; a caller with
//! no resolvable kind counts as unfenced (fail closed), and a request with no
//! caller is refused.

use std::io::Read;
use std::path::PathBuf;
use std::process::{Command as Proc, Stdio};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use super::{check_orchestration, fence_for, refuse, resolve, Decision, Fence, Op};
use crate::cli::{CliError, Command, Env, Registry};
use crate::daemon::Daemon;
use crate::rpc::{self, RpcError};

pub const VERB: &str = "push";

fn bad(m: impl Into<String>) -> RpcError {
    RpcError::new(rpc::BAD_REQUEST, m)
}

fn internal(e: impl std::fmt::Display) -> RpcError {
    RpcError::new("internal", e.to_string())
}

/// The caller's project repo path, if it has one.
fn project_repo(conn: &rusqlite::Connection, caller: &str) -> Option<String> {
    let a = resolve(conn, caller).ok().flatten()?;
    conn.query_row(
        "SELECT repo_path FROM project WHERE id = ?1",
        [a.project_id?],
        |r| r.get(0),
    )
    .ok()
    .flatten()
}

/// A git command with the user's and the system's config out of the way.
pub(crate) fn git(repo: &str) -> Proc {
    let mut p = Proc::new("git");
    p.arg("-C")
        .arg(repo)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_TERMINAL_PROMPT", "0");
    p
}

/// A plain branch name: no refspec syntax, and git accepts it as a branch.
pub(crate) fn plain_branch(repo: &str, b: &str) -> bool {
    !b.is_empty()
        && !b.starts_with(['-', '+', '^', ':'])
        && !b.contains([':', '+', '^', '*', '?', '[', '\\'])
        && git(repo)
            .args(["check-ref-format", "--branch", b])
            .output()
            .is_ok_and(|o| o.status.success() && String::from_utf8_lossy(&o.stdout).trim() == b)
}

/// A remote configured in `repo`, by name only (never a URL or path).
pub(crate) fn configured_remote(repo: &str, name: &str) -> bool {
    git(repo).arg("remote").output().is_ok_and(|o| {
        String::from_utf8_lossy(&o.stdout)
            .lines()
            .any(|l| l == name)
    })
}

/// Branch names compare case-insensitively: a case-insensitive remote or
/// filesystem makes `Main` and `main` the same ref.
pub(crate) fn same_branch(a: &str, b: &str) -> bool {
    a.to_lowercase() == b.to_lowercase()
}

/// A branch the verb never publishes: `main`, `master` (any case), the
/// remote's HEAD branch (`refs/remotes/<remote>/HEAD`, local only, may be
/// unset), or the repo's configured `init.defaultBranch`. Local refs only;
/// [`remote_default`] asks the remote itself.
pub(crate) fn protected_branch(repo: &str, remote: &str, b: &str) -> bool {
    let out = |args: &[&str]| {
        git(repo)
            .args(args)
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
    };
    same_branch(b, "main")
        || same_branch(b, "master")
        || out(&[
            "symbolic-ref",
            "--short",
            &format!("refs/remotes/{remote}/HEAD"),
        ])
        .and_then(|h| h.strip_prefix(&format!("{remote}/")).map(str::to_string))
        .is_some_and(|h| same_branch(&h, b))
        || out(&["config", "init.defaultBranch"]).is_some_and(|d| same_branch(&d, b))
}

/// What the remote itself says its default branch is.
#[derive(Debug, PartialEq)]
pub(crate) enum RemoteDefault {
    /// Every push URL's HEAD is a symref; these are the branches (deduplicated).
    Branches(Vec<String>),
    /// No push URL has any refs: there is no default branch to read yet. The
    /// remote's unborn HEAD is invisible to `ls-remote`, so this is unproven.
    Empty,
    /// Unproven: a read failed or timed out, a push URL is not stable under
    /// URL rewriting, HEAD names no branch, or the push URLs disagree on
    /// whether the remote is empty.
    Unknown,
}

/// How long one `ls-remote` may take before the remote counts as unproven.
const LS_REMOTE_TIMEOUT: Duration = Duration::from_secs(20);

/// Runs `cmd` with stdin closed and stderr dropped, killing it after
/// `limit`. `None` on a spawn failure, a non-zero exit or a timeout.
pub(crate) fn stdout_within(mut cmd: Proc, limit: Duration) -> Option<Vec<u8>> {
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let mut pipe = child.stdout.take()?;
    // Drain on a thread so a long listing cannot fill the pipe and stall git.
    let reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = pipe.read_to_end(&mut buf);
        buf
    });
    let deadline = Instant::now() + limit;
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break Some(s),
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(10));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
        }
    };
    let buf = reader.join().ok()?;
    status.filter(|s| s.success()).map(|_| buf)
}

/// The URLs `git push <remote>` goes to: `pushurl` entries, else the URL, with
/// `insteadOf` / `pushInsteadOf` applied. Not the fetch URL.
fn push_urls(repo: &str, remote: &str) -> Option<Vec<String>> {
    let out = stdout_within(
        {
            let mut c = git(repo);
            c.args(["remote", "get-url", "--push", "--all", "--", remote]);
            c
        },
        LS_REMOTE_TIMEOUT,
    )?;
    let urls: Vec<String> = String::from_utf8_lossy(&out)
        .lines()
        .map(str::to_string)
        .collect();
    (!urls.is_empty() && urls.iter().all(|u| !u.is_empty())).then_some(urls)
}

/// `u` survives git's URL handling unchanged, so `ls-remote -- u` and
/// `push -- remote` cannot land on different repos: no `insteadOf` rewrites
/// it again, no `pushInsteadOf` prefix matches it, and it is not a remote name.
fn url_is_stable(repo: &str, u: &str) -> bool {
    let clean = git(repo)
        .args([
            "config",
            "--null",
            "--get-regexp",
            r"^url\..*\.(insteadof|pushinsteadof)$",
        ])
        .output()
        .is_ok_and(|o| match o.status.code() {
            // Exit 1: no rewrite rules at all.
            Some(1) => true,
            Some(0) => String::from_utf8_lossy(&o.stdout)
                .split('\0')
                .filter_map(|e| e.split_once('\n'))
                .all(|(_, prefix)| !u.starts_with(prefix)),
            _ => false,
        });
    clean
        && stdout_within(
            {
                let mut c = git(repo);
                c.args(["ls-remote", "--get-url", "--", u]);
                c
            },
            LS_REMOTE_TIMEOUT,
        )
        .is_some_and(|b| String::from_utf8_lossy(&b).trim_end_matches('\n') == u)
}

/// `ls-remote --symref` against one URL.
fn url_default(repo: &str, url: &str) -> RemoteDefault {
    if !url_is_stable(repo, url) {
        return RemoteDefault::Unknown;
    }
    let Some(out) = stdout_within(
        {
            let mut c = git(repo);
            c.args(["ls-remote", "--symref", "--", url]);
            c
        },
        LS_REMOTE_TIMEOUT,
    ) else {
        return RemoteDefault::Unknown;
    };
    let text = String::from_utf8_lossy(&out);
    if text.trim().is_empty() {
        return RemoteDefault::Empty;
    }
    text.lines()
        .find_map(|l| l.strip_prefix("ref: refs/heads/")?.strip_suffix("\tHEAD"))
        .map_or(RemoteDefault::Unknown, |b| {
            RemoteDefault::Branches(vec![b.to_string()])
        })
}

/// Resolves the URLs `git push <remote>` goes to, once, and asks each of them
/// (`ls-remote --symref`, not the fetch URL) for its default branch. Any
/// unproven URL makes the answer [`Unknown`](RemoteDefault::Unknown). The URLs
/// come back with the answer so the caller pushes to exactly what was checked
/// ([`push_checked`]); they are empty when the answer is `Unknown`.
pub(crate) fn resolve_remote(repo: &str, remote: &str) -> (Vec<String>, RemoteDefault) {
    let Some(urls) = push_urls(repo, remote) else {
        return (Vec::new(), RemoteDefault::Unknown);
    };
    let (mut branches, mut empty) = (Vec::<String>::new(), 0);
    for u in &urls {
        match url_default(repo, u) {
            RemoteDefault::Unknown => return (Vec::new(), RemoteDefault::Unknown),
            RemoteDefault::Empty => empty += 1,
            RemoteDefault::Branches(b) => branches.extend(b),
        }
    }
    let default = match (branches.is_empty(), empty) {
        (true, _) => RemoteDefault::Empty,
        (false, 0) => {
            branches.sort();
            branches.dedup();
            RemoteDefault::Branches(branches)
        }
        (false, _) => RemoteDefault::Unknown,
    };
    (urls, default)
}

/// Pushes `refs/heads/<branch>` to exactly the `urls` that were checked, as a
/// plain non-forcing `git push -- <url> <refspec>`. The push runs from a
/// scratch bare repo with an empty config (the work repo's objects through
/// `alternates`, the branch tip pinned once), so no `insteadOf`, `pushurl`,
/// `remote.<name>.*` or `url.*` change made after the check can send it
/// anywhere else. The URLs are used as given, relative to `repo` as when they
/// were read. First failure stops, as `git push <remote>` does.
pub(crate) fn push_checked(repo: &str, urls: &[String], branch: &str) -> Result<(), String> {
    let out = |args: &[&str]| {
        git(repo)
            .args(args)
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .filter(|s| !s.is_empty())
    };
    let tip = out(&[
        "rev-parse",
        "--verify",
        "-q",
        &format!("refs/heads/{branch}^{{commit}}"),
    ])
    .ok_or_else(|| format!("branch {branch} does not exist"))?;
    let objects = out(&[
        "rev-parse",
        "--path-format=absolute",
        "--git-path",
        "objects",
    ])
    .ok_or("cannot find the repo's object store")?;
    let scratch = std::env::temp_dir().join(format!(
        "qq-push-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos())
    ));
    let result = push_from_scratch(repo, &scratch, &objects, &tip, urls, branch);
    let _ = std::fs::remove_dir_all(&scratch);
    result
}

/// The credential helpers from the user's global config, as `(key, value)`
/// pairs: `credential.helper` and `credential.<url>.helper`. Nothing else is read.
fn global_credential_helpers() -> Vec<(String, String)> {
    let mut c = Proc::new("git");
    c.args([
        "config",
        "--global",
        "--null",
        "--get-regexp",
        r"^credential\.(.*\.)?helper$",
    ])
    .env("GIT_TERMINAL_PROMPT", "0");
    stdout_within(c, LS_REMOTE_TIMEOUT)
        .map(|o| parse_helpers(&String::from_utf8_lossy(&o)))
        .unwrap_or_default()
}

/// Parses `git config --null --get-regexp` output (`key\nvalue\0`).
fn parse_helpers(text: &str) -> Vec<(String, String)> {
    text.split('\0')
        .filter_map(|e| e.split_once('\n'))
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

/// The `git push` for one URL from the scratch repo: system and global config
/// stay out, and only the given credential helpers come in, as `-c` args.
fn push_command(
    scratch: &std::path::Path,
    repo: &str,
    url: &str,
    refspec: &str,
    helpers: &[(String, String)],
) -> Proc {
    let mut c = Proc::new("git");
    c.arg("--git-dir").arg(scratch).current_dir(repo);
    // An empty first value resets the helper list; each global helper follows.
    if !helpers.is_empty() {
        c.args(["-c", "credential.helper="]);
    }
    for (k, v) in helpers {
        c.arg("-c").arg(format!("{k}={v}"));
    }
    c.env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_TERMINAL_PROMPT", "0")
        .args(["push", "--", url, refspec]);
    c
}

fn push_from_scratch(
    repo: &str,
    scratch: &std::path::Path,
    objects: &str,
    tip: &str,
    urls: &[String],
    branch: &str,
) -> Result<(), String> {
    let s = scratch.to_string_lossy().into_owned();
    let run = |mut c: Proc| -> Result<(), String> {
        let o = c.output().map_err(|e| e.to_string())?;
        o.status
            .success()
            .then_some(())
            .ok_or_else(|| String::from_utf8_lossy(&o.stderr).trim().to_string())
    };
    std::fs::create_dir_all(scratch).map_err(|e| e.to_string())?;
    run({
        let mut c = git(&s);
        c.args(["init", "-q", "--bare"]);
        c
    })?;
    std::fs::write(
        scratch.join("objects/info/alternates"),
        format!("{objects}\n"),
    )
    .map_err(|e| e.to_string())?;
    run({
        let mut c = git(&s);
        c.args(["update-ref", &format!("refs/heads/{branch}"), tip]);
        c
    })?;
    let refspec = format!("refs/heads/{branch}:refs/heads/{branch}");
    let helpers = global_credential_helpers();
    for u in urls {
        run({ push_command(scratch, repo, u, &refspec, &helpers) })?;
    }
    Ok(())
}

/// The fence of the harness the caller was spawned with (pinned on its row);
/// a row from before the pin resolves its kind. Unresolvable: unfenced.
pub fn fence_of(conn: &rusqlite::Connection, caller: &str) -> Fence {
    resolve(conn, caller)
        .ok()
        .flatten()
        .and_then(|a| crate::task::spawn::live_harness(conn, a.id))
        .map_or(Fence::Unfenced, |h| fence_for(h.driver))
}

/// Registers `push {repo, remote, branch}` -> `{pushed: true}`.
pub fn register(daemon: &mut Daemon, runs_dir: PathBuf) -> Result<(), String> {
    daemon.register_verb(
        VERB,
        Box::new(move |_, req| {
            let a = &req.args;
            let arg = |k: &str| {
                a.get(k)
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                    .ok_or_else(|| bad(format!("missing argument '{k}'")))
            };
            let (repo, remote, branch) = (arg("repo")?, arg("remote")?, arg("branch")?);
            let conn =
                crate::db::open(&runs_dir.join(crate::db::path::DB_FILE)).map_err(internal)?;
            let Some(caller) = req.caller.as_deref() else {
                return Err(RpcError::new("refused", "push needs an identified caller"));
            };
            let fence = fence_of(&conn, caller);
            let deny = |reason: &str, target: &str| -> Result<Value, RpcError> {
                match refuse(&conn, caller, "push", target, reason).map_err(internal)? {
                    Decision::Refused(r) => Err(RpcError::new(
                        "refused",
                        format!("{} -> {}: {}", r.caller, r.target, r.reason),
                    )),
                    Decision::Allowed => unreachable!("refuse always refuses"),
                }
            };
            if let Decision::Refused(r) =
                check_orchestration(&conn, caller, Op::Push, fence).map_err(internal)?
            {
                return Err(RpcError::new(
                    "refused",
                    format!("{} -> {}: {}", r.caller, r.target, r.reason),
                ));
            }
            let own = project_repo(&conn, caller)
                .and_then(|p| std::fs::canonicalize(p).ok())
                .zip(std::fs::canonicalize(repo).ok())
                .is_some_and(|(p, r)| p == r);
            if !own {
                return deny("repo is not the caller's project repo", repo);
            }
            if !plain_branch(repo, branch) {
                return deny("branch must be a plain branch name", branch);
            }
            if !configured_remote(repo, remote) {
                return deny("remote is not configured in the repo", remote);
            }
            // Resolve the push URLs once; the push goes to these URLs, not to
            // the remote name, so a config flip after this read changes nothing.
            // An empty remote has no readable default (its unborn HEAD is
            // invisible), so it is refused like an unproven one.
            let (urls, default) = resolve_remote(repo, remote);
            if protected_branch(repo, remote, branch)
                || match default {
                    RemoteDefault::Branches(d) => d.iter().any(|d| same_branch(d, branch)),
                    RemoteDefault::Empty | RemoteDefault::Unknown => true,
                }
            {
                return deny("branch is the repo's main or default branch", branch);
            }
            push_checked(repo, &urls, branch).map_err(|e| RpcError::new("push_failed", e))?;
            Ok(json!({ "pushed": true, "remote": remote, "branch": branch }))
        }),
    )
}

fn push_cli(env: &Env, args: &[String]) -> Result<String, CliError> {
    let [repo, remote, branch] = args else {
        return Err(CliError::new(
            2,
            "usage: quaqua push <repo> <remote> <branch>",
        ));
    };
    crate::cli::rpc_call(
        env,
        VERB,
        json!({ "repo": repo, "remote": remote, "branch": branch }),
    )
    .map(|v| serde_json::to_string_pretty(&v).unwrap_or_default())
}

pub(super) fn install(registry: &mut Registry, daemon: &mut Daemon) -> Result<(), String> {
    register(daemon, crate::db::path::default_runs_dir())?;
    registry.register(Command {
        name: "push",
        help: "push <repo> <remote> <branch>: git push through the daemon (fenced Lead only)",
        run: push_cli,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stdout_within_kills_a_hang_and_keeps_fast_output() {
        let mut slow = Proc::new("sleep");
        slow.arg("30");
        let t = Instant::now();
        assert!(stdout_within(slow, Duration::from_millis(200)).is_none());
        assert!(
            t.elapsed() < Duration::from_secs(5),
            "did not kill the hang"
        );
        let mut fast = Proc::new("echo");
        fast.arg("hi");
        assert_eq!(
            stdout_within(fast, Duration::from_secs(5)).unwrap(),
            b"hi\n"
        );
    }

    fn argv(c: &Proc) -> Vec<String> {
        c.get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn push_carries_global_helpers_and_keeps_config_isolated() {
        let h = parse_helpers(
            "credential.helper\n!/usr/bin/gh auth git-credential\0credential.https://github.com.helper\nstore\0",
        );
        let c = push_command(
            std::path::Path::new("/s"),
            "/r",
            "https://github.com/o/r",
            "a:b",
            &h,
        );
        let a = argv(&c);
        let i = a.iter().position(|x| x == "push").unwrap();
        assert_eq!(
            a[..i],
            [
                "--git-dir",
                "/s",
                "-c",
                "credential.helper=",
                "-c",
                "credential.helper=!/usr/bin/gh auth git-credential",
                "-c",
                "credential.https://github.com.helper=store",
            ]
        );
        assert_eq!(a[i..], ["push", "--", "https://github.com/o/r", "a:b"]);
        let env: Vec<_> = c.get_envs().collect();
        let get = |k: &str| env.iter().find(|(n, _)| *n == k).and_then(|(_, v)| *v);
        assert_eq!(get("GIT_CONFIG_GLOBAL").unwrap(), "/dev/null");
        assert_eq!(get("GIT_CONFIG_NOSYSTEM").unwrap(), "1");
    }
}
