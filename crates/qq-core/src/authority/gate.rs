//! The `gate` verb: the Claude Code PreToolUse(Bash) hook, ported from
//! the legacy engine. Stateless: it tokenizes the hook's command and
//! blocks what the daemon alone may do under R11 — a herdr pane, tab or
//! workspace spawn/move/close, any `git push`, and a `gh` merge. Everything
//! else is allowed. It fails closed: empty input, a read error, a body that
//! is not JSON, or a missing or non-string `tool_input.command` all block.
//!
//! Not ported: the old gate's `ask` prompts, `pkill -f`, removed worker
//! scripts, and the CoS-pane lookup (every pane close is blocked instead: the
//! daemon is the door).

use super::tokenizer::tokenize;
use crate::cli::{CliError, Command, Env};

/// Exit status and stderr message of a block (Claude Code treats 2 as "refuse").
pub const BLOCK_EXIT: i32 = 2;

const PANE_MESSAGE: &str = "blocked: spawn/move panes and workspaces only through the quaqua daemon (quaqua call task.spawn / grid verbs)";
const PUSH_MESSAGE: &str =
    "blocked: git push only through the quaqua daemon (the daemon is the one door for push)";
const MERGE_MESSAGE: &str =
    "blocked: merge only through the quaqua daemon (the daemon is the one door for merge)";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    Allow,
    Block(&'static str),
}

/// Decide for the hook's stdin JSON. Anything the gate cannot read as a
/// command string blocks.
pub fn evaluate(hook_json: &str) -> Verdict {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(hook_json) else {
        return Verdict::Block(PANE_MESSAGE);
    };
    match v
        .get("tool_input")
        .and_then(|t| t.get("command"))
        .and_then(|c| c.as_str())
    {
        Some(cmd) if !cmd.trim().is_empty() => check(cmd),
        _ => Verdict::Block(PANE_MESSAGE),
    }
}

/// [`evaluate`] over a reader (the hook's stdin); a read error blocks.
pub fn evaluate_reader<R: std::io::Read>(mut r: R) -> Verdict {
    let mut s = String::new();
    match r.read_to_string(&mut s) {
        Ok(_) => evaluate(&s),
        Err(_) => Verdict::Block(PANE_MESSAGE),
    }
}

/// Check one shell command: every simple command in it, nested ones included.
pub fn check(command: &str) -> Verdict {
    let segs = tokenize(command);
    for seg in &segs {
        if let v @ Verdict::Block(_) = check_words(seg) {
            return v;
        }
    }
    // A shell that reads its script from stdin (`| bash`, `bash -s`, a
    // here-string or heredoc) hides the script from the tokenizer: block when
    // the command text names a push.
    if mentions_push(command) && segs.iter().any(|s| reads_stdin_script(s)) {
        return Verdict::Block(PUSH_MESSAGE);
    }
    Verdict::Allow
}

fn mentions_push(text: &str) -> bool {
    let words: Vec<&str> = text
        .split(|c: char| !c.is_ascii_alphanumeric() && c != '_' && c != '-')
        .collect();
    words.contains(&"git") && words.contains(&"push")
}

fn is_shell(base: &str) -> bool {
    matches!(
        base,
        "bash" | "sh" | "dash" | "zsh" | "ksh" | "mksh" | "ash" | "fish"
    )
}

/// The words of a shell invocation (busybox applet unwrapped), if `w` is one.
fn shell_args(w: &[String]) -> Option<&[String]> {
    let i = w.iter().position(|x| !keyword(x) && !is_assignment(x))?;
    let base = w[i].rsplit('/').next().unwrap_or(&w[i]);
    let a = &w[i + 1..];
    if is_shell(base) {
        Some(a)
    } else if base == "busybox" && a.first().is_some_and(|x| is_shell(x)) {
        Some(&a[1..])
    } else {
        None
    }
}

/// A shell with no `-c` payload and no script file reads stdin.
fn reads_stdin_script(w: &[String]) -> bool {
    let Some(a) = shell_args(w) else {
        return false;
    };
    if a.iter().any(|x| is_c_flag(x)) {
        return false;
    }
    a.iter().all(|x| x.starts_with('-')) || a.iter().any(|x| x == "-s")
}

fn keyword(w: &str) -> bool {
    matches!(
        w,
        "if" | "then"
            | "else"
            | "elif"
            | "fi"
            | "for"
            | "in"
            | "while"
            | "until"
            | "do"
            | "done"
            | "case"
            | "esac"
            | "select"
            | "function"
            | "{"
            | "}"
            | "!"
            | "coproc"
    )
}

/// `^[A-Za-z_][A-Za-z0-9_]*=`
fn is_assignment(w: &str) -> bool {
    let Some((name, _)) = w.split_once('=') else {
        return false;
    };
    let mut c = name.chars();
    matches!(c.next(), Some(f) if f.is_ascii_alphabetic() || f == '_')
        && c.all(|x| x.is_ascii_alphanumeric() || x == '_')
}

/// `^-[a-z]*c$`: the `bash -c` payload flag.
fn is_c_flag(w: &str) -> bool {
    w.len() >= 2
        && w.starts_with('-')
        && w.ends_with('c')
        && w[1..w.len() - 1].chars().all(|c| c.is_ascii_lowercase())
}

fn single_letter_flag(w: &str) -> Option<char> {
    let mut c = w.chars();
    match (c.next(), c.next(), c.next()) {
        (Some('-'), Some(f), None) if f.is_ascii_alphabetic() => Some(f),
        _ => None,
    }
}

fn guarded_pair(a: &str, b: &str) -> bool {
    matches!(
        (a, b),
        ("pane", "split")
            | ("pane", "move")
            | ("pane", "close")
            | ("tab", "create")
            | ("tab", "close")
            | ("workspace", "create")
            | ("workspace", "close")
            | ("agent", "start")
    )
}

fn check_words(w: &[String]) -> Verdict {
    let Some(i) = w.iter().position(|x| !keyword(x) && !is_assignment(x)) else {
        return Verdict::Allow;
    };
    let base = w[i].rsplit('/').next().unwrap_or(&w[i]);
    let a = &w[i + 1..];
    match base {
        "herdr" => check_herdr(a),
        "git" => check_git(a),
        "gh" => check_gh(a),
        b if is_shell(b) => check_shell(a),
        "busybox" if a.first().is_some_and(|x| is_shell(x)) => check_shell(&a[1..]),
        "eval" if !a.is_empty() => check(&a.join(" ")),
        "find" => check_find(a),
        "sudo" | "env" | "command" | "exec" | "nice" | "time" | "nohup" | "setsid" | "stdbuf"
        | "xargs" | "builtin" | "chrt" | "ionice" | "taskset" | "timeout" => wrapper(base, a),
        _ => Verdict::Allow,
    }
}

fn check_shell(a: &[String]) -> Verdict {
    for k in 0..a.len() {
        if is_c_flag(&a[k]) && k + 1 < a.len() {
            return check(&a[k + 1]);
        }
    }
    Verdict::Allow
}

/// `find … -exec|-execdir|-ok|-okdir CMD … ;|+`: CMD is checked.
fn check_find(a: &[String]) -> Verdict {
    for (k, w) in a.iter().enumerate() {
        if matches!(w.as_str(), "-exec" | "-execdir" | "-ok" | "-okdir") {
            let cmd: Vec<String> = a[k + 1..]
                .iter()
                .take_while(|x| x.as_str() != ";" && x.as_str() != "+")
                .cloned()
                .collect();
            if let v @ Verdict::Block(_) = check_words(&cmd) {
                return v;
            }
        }
    }
    Verdict::Allow
}

/// `gh pr merge`, a merge endpoint through `gh api`, or a merge mutation.
fn check_gh(a: &[String]) -> Verdict {
    let pr = a.iter().position(|x| x == "pr");
    if pr.is_some_and(|p| a[p + 1..].iter().any(|x| x == "merge")) {
        return Verdict::Block(MERGE_MESSAGE);
    }
    if a.iter().any(|x| x == "api") {
        let merge_path = a.iter().any(|x| {
            let path = x.split('?').next().unwrap_or(x).trim_end_matches('/');
            !x.starts_with('-') && (path.ends_with("/merge") || path.ends_with("/merges"))
        });
        let explicit_get = a.windows(2).any(|w| {
            matches!(w[0].as_str(), "-X" | "--method") && w[1].eq_ignore_ascii_case("GET")
        }) || a.iter().any(|x| x.eq_ignore_ascii_case("-XGET"));
        if (merge_path && !explicit_get) || a.iter().any(|x| x.contains("mergePullRequest")) {
            return Verdict::Block(MERGE_MESSAGE);
        }
    }
    Verdict::Allow
}

/// A guarded pair counts only as the command's verb: every word before it is
/// a global option or that option's value, and no `--help` follows.
fn check_herdr(a: &[String]) -> Verdict {
    if matches!(a.first().map(String::as_str), Some("--help" | "-h")) {
        return Verdict::Allow;
    }
    let Some(first) = (0..a.len().saturating_sub(1)).find(|&k| guarded_pair(&a[k], &a[k + 1]))
    else {
        return Verdict::Allow;
    };
    let is_verb = (0..first).all(|k| a[k].starts_with('-') || (k > 0 && a[k - 1].starts_with('-')));
    let help = a.get(first + 2).is_some_and(|x| x == "--help" || x == "-h");
    if is_verb && !help {
        Verdict::Block(PANE_MESSAGE)
    } else {
        Verdict::Allow
    }
}

/// `git [global options] push …`: every push is blocked, `--help` aside.
fn check_git(a: &[String]) -> Verdict {
    let mut i = 0;
    while i < a.len() {
        let w = a[i].as_str();
        if w == "-c" || (w.starts_with("-c") && w.len() > 2) {
            let cfg = if w == "-c" { a.get(i + 1) } else { Some(&a[i]) };
            let cfg = cfg.map(|c| c.trim_start_matches("-c")).unwrap_or("");
            if let Some(rest) = cfg.strip_prefix("alias.") {
                let body = rest.split_once('=').map_or("", |x| x.1);
                if body
                    .split_whitespace()
                    .any(|t| t.trim_start_matches('!') == "push")
                {
                    return Verdict::Block(PUSH_MESSAGE);
                }
            }
            i += if w == "-c" { 2 } else { 1 };
        } else if w == "-C" {
            i += 2;
        } else if w == "--help" || w == "-h" {
            return Verdict::Allow;
        } else if matches!(
            w,
            "--git-dir" | "--work-tree" | "--namespace" | "--exec-path"
        ) {
            i += 2;
        } else if w.starts_with('-') {
            i += 1;
        } else {
            break;
        }
    }
    if a.get(i).map(String::as_str) != Some("push") {
        return Verdict::Allow;
    }
    if a[i + 1..].iter().any(|x| x == "--help" || x == "-h") {
        return Verdict::Allow;
    }
    Verdict::Block(PUSH_MESSAGE)
}

fn wrapper(base: &str, a: &[String]) -> Verdict {
    let value_flags: &str = match base {
        "sudo" => " u g h p C T U R t D ",
        "env" => " u C S ",
        "nice" => " n ",
        "timeout" => " s k ",
        "xargs" => " I n L P s d E a ",
        _ => " ",
    };
    let mut k = 0;
    let mut duration_pending = base == "timeout";
    while k < a.len() {
        if let Some(f) = single_letter_flag(&a[k]) {
            if value_flags.contains(&format!(" {f} ")) {
                // `env -S 'cmd args'`: the value is a command line.
                if base == "env" && f == 'S' {
                    if let Some(p) = a.get(k + 1) {
                        if let v @ Verdict::Block(_) = check(p) {
                            return v;
                        }
                    }
                }
                k += 2;
                continue;
            }
        }
        if let Some(p) = a[k].strip_prefix("--split-string=") {
            if let v @ Verdict::Block(_) = check(p) {
                return v;
            }
        }
        if a[k].starts_with('-') || is_assignment(&a[k]) {
            k += 1;
            continue;
        }
        if duration_pending {
            duration_pending = false;
            k += 1;
            continue;
        }
        return check_words(&a[k..]);
    }
    Verdict::Allow
}

/// The rule-drift warning for an allowed `quaqua|quaqua worker|lead` dispatch from a
/// session whose rule files changed since it started (issue #72): which files
/// to re-read. `None` when the command is no dispatch, the hook JSON names no
/// session, or nothing changed. Never blocks; the caller prints it.
fn drift_warning(ws_root: &std::path::Path, hook_json: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(hook_json).ok()?;
    let cmd = v.get("tool_input")?.get("command")?.as_str()?;
    if !crate::memory::is_dispatch(&tokenize(cmd)) {
        return None;
    }
    let sid = v.get("session_id")?.as_str()?;
    let changed = crate::memory::drift(ws_root, sid);
    (!changed.is_empty()).then(|| crate::memory::drift_message(&changed))
}

fn run(env: &Env, args: &[String]) -> Result<String, CliError> {
    let json = match args {
        [flag, json] if flag == "--json" => json.clone(),
        [] => {
            let mut s = String::new();
            match std::io::Read::read_to_string(&mut std::io::stdin(), &mut s) {
                Ok(_) => s,
                // A read error blocks, like any input the gate cannot read.
                Err(_) => return Err(CliError::new(BLOCK_EXIT, PANE_MESSAGE)),
            }
        }
        _ => return Err(CliError::new(2, "usage: quaqua gate [--json <hook-json>]")),
    };
    match evaluate(&json) {
        Verdict::Allow => {
            if let Some(ws) = env.runs_dir.parent() {
                if let Some(msg) = drift_warning(ws, &json) {
                    eprintln!("quaqua: {msg}");
                }
            }
            Ok(String::new())
        }
        Verdict::Block(msg) => Err(CliError::new(BLOCK_EXIT, msg)),
    }
}

pub(super) fn command() -> Command {
    Command {
        name: "gate",
        help: "PreToolUse hook: block pane spawn/move and git push",
        run,
    }
}

#[cfg(test)]
mod drift_tests {
    use super::*;

    #[test]
    fn a_dispatch_from_a_drifted_session_is_warned_and_still_allowed() {
        let ws = std::env::temp_dir().join(format!("qq-gate-drift-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&ws);
        std::fs::create_dir_all(ws.join("rules/_all")).unwrap();
        std::fs::write(ws.join("AGENTS.md"), "v1").unwrap();
        crate::memory::record_session(&ws, "s1", "startup");
        let hook = |c: &str| {
            serde_json::json!({"session_id": "s1", "tool_input": {"command": c}}).to_string()
        };
        std::fs::write(ws.join("rules/_all/new.md"), "rule").unwrap();
        assert!(
            drift_warning(&ws, &hook("quaqua status")).is_none(),
            "not a dispatch"
        );
        let msg = drift_warning(&ws, &hook("cd x && quaqua worker spawn a b c")).unwrap();
        assert!(msg.contains("rules/_all/new.md"), "{msg}");
        assert!(
            drift_warning(&ws, &hook("quaqua lead prompt a")).is_none(),
            "warned once"
        );
        // The verdict and the exit code do not depend on the warning.
        std::fs::write(ws.join("AGENTS.md"), "v2").unwrap();
        let env = Env {
            runs_dir: ws.join("runs"),
        };
        let out = run(&env, &["--json".into(), hook("quaqua lead prompt a")]);
        assert_eq!(out.unwrap(), "");
        let _ = std::fs::remove_dir_all(&ws);
    }
}
