//! The `gate` verb: the Claude Code PreToolUse(Bash) hook, ported from
//! `crates/quaqua/src/gate`. Stateless: it tokenizes the hook's command and
//! blocks what the daemon alone may do under R11 — a herdr pane, tab or
//! workspace spawn/move, and any `git push`. Everything else is allowed.
//!
//! Not ported: the old gate's `ask`/`deny` outward prompts (gh merge, force
//! push wording, `pkill -f`, removed worker scripts) and the CoS-pane lookup;
//! they belong to the old CLI's surface, not to the daemon's doors.

use super::tokenizer::tokenize;
use crate::cli::{CliError, Command, Env};

/// Exit status and stderr message of a block (Claude Code treats 2 as "refuse").
pub const BLOCK_EXIT: i32 = 2;

const PANE_MESSAGE: &str = "blocked: spawn/move panes and workspaces only through the qq daemon (qq call task.spawn / grid verbs)";
const PUSH_MESSAGE: &str =
    "blocked: git push only through the qq daemon (the daemon is the one door for push)";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    Allow,
    Block(&'static str),
}

/// Decide for the hook's stdin JSON. Empty input allows; input that is not
/// JSON fails closed (the gate cannot read the command it guards).
pub fn evaluate(hook_json: &str) -> Verdict {
    if hook_json.trim().is_empty() {
        return Verdict::Allow;
    }
    let Ok(v) = serde_json::from_str::<serde_json::Value>(hook_json) else {
        return Verdict::Block(PANE_MESSAGE);
    };
    let cmd = v
        .get("tool_input")
        .and_then(|t| t.get("command"))
        .and_then(|c| c.as_str())
        .unwrap_or("");
    check(cmd)
}

/// Check one shell command: every simple command in it, nested ones included.
pub fn check(command: &str) -> Verdict {
    for seg in tokenize(command) {
        if let v @ Verdict::Block(_) = check_words(&seg) {
            return v;
        }
    }
    Verdict::Allow
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
            | ("tab", "create")
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
        "bash" | "sh" | "dash" | "zsh" => {
            for k in 0..a.len() {
                if is_c_flag(&a[k]) && k + 1 < a.len() {
                    return check(&a[k + 1]);
                }
            }
            Verdict::Allow
        }
        "eval" if !a.is_empty() => check(&a.join(" ")),
        "sudo" | "env" | "command" | "exec" | "nice" | "time" | "nohup" | "setsid" | "stdbuf"
        | "xargs" | "builtin" | "chrt" | "ionice" | "taskset" => wrapper(base, a),
        _ => Verdict::Allow,
    }
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
        if w == "-C" || w == "-c" {
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
        _ => " ",
    };
    let mut k = 0;
    while k < a.len() {
        if let Some(f) = single_letter_flag(&a[k]) {
            if value_flags.contains(&format!(" {f} ")) {
                k += 2;
                continue;
            }
        }
        if a[k].starts_with('-') || is_assignment(&a[k]) {
            k += 1;
            continue;
        }
        return check_words(&a[k..]);
    }
    Verdict::Allow
}

fn run(_: &Env, args: &[String]) -> Result<String, CliError> {
    let input = match args {
        [flag, json] if flag == "--json" => json.clone(),
        [] => {
            use std::io::Read;
            let mut s = String::new();
            let _ = std::io::stdin().read_to_string(&mut s);
            s
        }
        _ => return Err(CliError::new(2, "usage: qq gate [--json <hook-json>]")),
    };
    match evaluate(&input) {
        Verdict::Allow => Ok(String::new()),
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
