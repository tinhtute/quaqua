//! The `gate` verb: the Claude Code PreToolUse hook (Bash, and Write/Edit on
//! the live instruction files). Stateless: it tokenizes the hook's command and
//! sorts what it finds in three bins.
//!
//! - Hard blocks, for everyone: a herdr pane, tab or workspace spawn/move/close,
//!   any `git push` that is not a pure branch delete, a `gh` merge (by any
//!   spelling), deleting main/master, and a ref or contents write that lands on
//!   main/master.
//! - The approval list, decided by caller and tier (R7, R14, R5): branch delete,
//!   `git branch -m|-M|-f` on main/master, `git tag`, `gh release create`,
//!   history rewrite on shared refs, the #297 bypass spellings (alias push,
//!   `gh api` ref/contents writes off main, `query=@file`), pane input and
//!   `grid_role` tokens, Bash naming `runs/qq.db`, and Write/Edit of the live
//!   AGENTS.md, SOUL.md and identity.md. Only a hit asks the daemon who is
//!   calling (`caller.lookup`); only the CoS gets past `deny`, and the tier
//!   table (`tier.rs`) says allow or ask. No daemon answer is `deny`.
//! - The two memory files (R4), `USER.md` and `memory/rules.md` of the live
//!   workspace: a Write/Edit to either, or a Bash command that writes one (a
//!   redirect onto it, or a non-read-only command naming it), asks the daemon
//!   who is calling. Only the CoS passes, with no ask; every other caller, and
//!   no daemon answer, is `deny`. The one writer is the daemon verb
//!   `memory.write`, which is also the only door that journals.
//! - Everything else is allowed, with no daemon call.
//!
//! A tripwire, not a fence: a same-uid process can still route around it. It
//! fails closed: empty input, a read error, a body that is not JSON, or a
//! missing or non-string `tool_input.command` all block.

use std::path::{Component, Path, PathBuf};

use super::tier::{self, Action, Decision, Tier};
use super::tokenizer::tokenize;
use crate::cli::{CliError, Command, Env};
use crate::daemon::caller::{self, Caller};

/// Exit status and stderr message of a block (Claude Code treats 2 as "refuse").
pub const BLOCK_EXIT: i32 = 2;

const PANE_MESSAGE: &str = "blocked: spawn/move panes and workspaces only through the quaqua daemon (quaqua call task.spawn / grid verbs)";
const PUSH_MESSAGE: &str =
    "blocked: git push only through the quaqua daemon (the daemon is the one door for push)";
const MERGE_MESSAGE: &str =
    "blocked: merge only through the quaqua daemon (the daemon is the one door for merge)";
const BRANCH_MESSAGE: &str = "blocked: deleting main or master is anh iu's call, never a worker's";
const NOT_COS_MESSAGE: &str =
    "blocked: this needs the CoS's approval; report to the CoS, who asks anh iu";
const NO_CALLER_MESSAGE: &str = "blocked: the quaqua daemon did not identify the caller (daemon down or unmapped peer); start the daemon, or report to the CoS";
const MEMORY_MESSAGE: &str = "blocked: USER.md and memory/rules.md are written only by the CoS, through the daemon's memory.write verb (the cos-memory tools); report what to remember to the CoS";
const TIER_DENY_MESSAGE: &str = "blocked: pushing to main and force-pushing are anh iu's call";

const ASK_BRANCH_DELETE: &str = "approval list: delete a branch";
const ASK_TRUNK_MOVE: &str = "approval list: rename or force-move main/master";
const ASK_TAG: &str = "approval list: git tag";
const ASK_RELEASE: &str = "approval list: gh release create";
const ASK_REWRITE: &str = "approval list: history rewrite on shared refs";
const ASK_BYPASS: &str = "approval list: a push or merge door by another name";
const ASK_INPUT: &str = "approval list: pane input or a grid_role token";
const ASK_DB: &str = "approval list: runs/qq.db belongs to the daemon";
const ASK_FILE: &str = "approval list: the CoS's live instruction files";
const ASK_MEMORY: &str = "the CoS's memory files";
const ASK_MCP: &str = "approval list: an MCP tool that sends, publishes or spends";

/// MCP tool names that send, publish or spend (docs/research/hermes-cos-memory/
/// mcp-outward-tools.md). `driver/claude.rs` and the template settings carry
/// the same regexes as PreToolUse matchers; `tests/hook_routes.rs` keeps them
/// in step.
const MCP_OUTWARD: [&str; 3] = [
    r"^mcp__plugin_tinyfish_tinyfish__(run_deep_research(_async)?|run_web_automation(_async)?|create_dataset|populate_dataset|top_up_wallet)$",
    r"^mcp__claude_ai_Claude_Data__share_(connector|live_queries)$",
    r"^mcp__claude_ai_(Asana|Atlassian|Box|Canva|Gmail|Google_Calendar|Google_Docs|Google_Drive|Google_Sheets|Google_Slides|Higgsfield|HubSpot|Intercom|Linear|Notion|Slack|monday_com)__.*(send|post|reply|publish|share|invite|comment|message|upload|generate)",
];

fn outward_mcp(tool: &str) -> bool {
    static RES: std::sync::OnceLock<Vec<regex::Regex>> = std::sync::OnceLock::new();
    RES.get_or_init(|| {
        MCP_OUTWARD
            .iter()
            .map(|r| regex::Regex::new(r).expect("MCP_OUTWARD regex"))
            .collect()
    })
    .iter()
    .any(|r| r.is_match(tool))
}

/// What the gate decided. `Ask` prompts the person; `Block` refuses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    Allow,
    Ask(&'static str),
    Block(&'static str),
}

/// An approval-list hit: a fixed action, or a branch delete whose class
/// (merged or not) is read from git only once the caller is known to be the CoS.
#[derive(Debug)]
enum Pending {
    Act(Action),
    BranchDelete(String),
    /// Only the CoS may do it, and the CoS needs no approval for it: no tier.
    CosOnly,
}

/// What a scan found: the first hard block, and every approval-list hit.
#[derive(Debug, Default)]
struct Found {
    block: Option<&'static str>,
    listed: Vec<(Pending, &'static str)>,
}

impl Found {
    fn pass() -> Self {
        Self::default()
    }

    fn block(m: &'static str) -> Self {
        Found {
            block: Some(m),
            listed: Vec::new(),
        }
    }

    fn act(a: Action, reason: &'static str) -> Self {
        Found {
            block: None,
            listed: vec![(Pending::Act(a), reason)],
        }
    }

    fn cos_only(reason: &'static str) -> Self {
        Found {
            block: None,
            listed: vec![(Pending::CosOnly, reason)],
        }
    }

    fn is_pass(&self) -> bool {
        self.block.is_none() && self.listed.is_empty()
    }

    fn absorb(&mut self, o: Found) {
        self.block = self.block.or(o.block);
        self.listed.extend(o.listed);
    }
}

/// Decide for the hook's stdin JSON with no daemon to ask: an approval-list hit
/// is denied. Anything the gate cannot read as a command string blocks.
pub fn evaluate(hook_json: &str) -> Verdict {
    decide_for(None, hook_json)
}

/// [`evaluate`] for a gate running in `env`'s workspace: an approval-list hit
/// asks that workspace's daemon who is calling.
pub fn evaluate_in(env: &Env, hook_json: &str) -> Verdict {
    decide_for(Some(env), hook_json)
}

/// The PreToolUse JSON that makes Claude Code prompt, even in auto mode.
pub fn ask_json(reason: &str) -> String {
    serde_json::json!({"hookSpecificOutput": {
        "hookEventName": "PreToolUse",
        "permissionDecision": "ask",
        "permissionDecisionReason": reason,
    }})
    .to_string()
}

fn decide_for(env: Option<&Env>, hook_json: &str) -> Verdict {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(hook_json) else {
        return Verdict::Block(PANE_MESSAGE);
    };
    let input = v.get("tool_input");
    let cwd = v.get("cwd").and_then(|c| c.as_str()).map(PathBuf::from);
    let tool = v.get("tool_name").and_then(|t| t.as_str());
    let found = match (input.and_then(|t| t.get("command")), tool) {
        // Spend, publish and send outward are the floor: ask at every tier.
        (_, Some(t)) if outward_mcp(t) => Found::act(Action::Floor, ASK_MCP),
        (Some(c), _) if c.is_string() => {
            let cmd = c.as_str().unwrap_or_default();
            if cmd.trim().is_empty() {
                return Verdict::Block(PANE_MESSAGE);
            }
            let mut f = scan(cmd);
            if let Some(env) = env {
                f.absorb(bash_memory(env, cmd, cwd.as_deref(), 0));
            }
            f
        }
        (_, Some("Write" | "Edit" | "MultiEdit" | "NotebookEdit")) => {
            let path = input
                .and_then(|t| t.get("file_path").or_else(|| t.get("notebook_path")))
                .and_then(|p| p.as_str())
                .filter(|p| !p.is_empty());
            match (path, env) {
                (None, _) => return Verdict::Block(PANE_MESSAGE),
                (Some(p), Some(env)) => edit_found(env, p, cwd.as_deref()),
                // No workspace to resolve against: treat as a hit and deny.
                (Some(_), None) => Found::act(Action::InstructionFile, ASK_FILE),
            }
        }
        (_, None | Some("Bash")) => return Verdict::Block(PANE_MESSAGE),
        _ => return Verdict::Allow,
    };
    judge(
        found,
        &|| env.map_or(Caller::Unknown, |e| caller::lookup(&e.runs_dir)),
        env.map(|e| e.runs_dir.as_path()),
        cwd.as_deref(),
    )
}

/// The verdict for a scan. The daemon is asked only when something is on the
/// approval list and nothing hard-blocked already.
fn judge(
    f: Found,
    who: &dyn Fn() -> Caller,
    runs_dir: Option<&Path>,
    cwd: Option<&Path>,
) -> Verdict {
    if let Some(m) = f.block {
        return Verdict::Block(m);
    }
    if f.listed.is_empty() {
        return Verdict::Allow;
    }
    let memory = f.listed.iter().any(|(p, _)| matches!(p, Pending::CosOnly));
    match who() {
        Caller::Cos => {}
        Caller::Other(_) if memory => return Verdict::Block(MEMORY_MESSAGE),
        Caller::Other(_) => return Verdict::Block(NOT_COS_MESSAGE),
        Caller::Unknown => return Verdict::Block(NO_CALLER_MESSAGE),
    }
    let dir = cwd.unwrap_or(Path::new("."));
    let ctx = runs_dir.map_or(Tier::High, |r| tier::effective_context_tier(r, dir, true));
    let mut ask = None;
    for (p, reason) in f.listed {
        let action = match p {
            Pending::Act(a) => a,
            Pending::BranchDelete(b) => tier::branch_delete_action(dir, &b),
            Pending::CosOnly => continue,
        };
        match tier::decide(true, action, ctx, ctx) {
            Decision::Deny => return Verdict::Block(TIER_DENY_MESSAGE),
            Decision::Ask => ask = Some(reason),
            Decision::Allow => {}
        }
    }
    ask.map_or(Verdict::Allow, Verdict::Ask)
}

/// A Write/Edit to the live AGENTS.md, SOUL.md or identity.md: the path is
/// resolved (cwd, `..`, symlinks) and compared under the CoS workspace root, so
/// a template or a repo AGENTS.md with the same basename is not a hit.
fn edit_found(env: &Env, file: &str, cwd: Option<&Path>) -> Found {
    let Some(root) = env.runs_dir.parent() else {
        return Found::act(Action::InstructionFile, ASK_FILE);
    };
    let root = resolve(root);
    let mut p = PathBuf::from(file);
    if p.is_relative() {
        p = cwd
            .map_or_else(
                || std::env::current_dir().unwrap_or_default(),
                Path::to_path_buf,
            )
            .join(p);
    }
    let p = resolve(&p);
    let live = ["AGENTS.md", "SOUL.md", "identity.md"]
        .iter()
        .any(|n| p == root.join(n));
    if live {
        Found::act(Action::InstructionFile, ASK_FILE)
    } else if is_memory_file(&root, &p) {
        Found::cos_only(ASK_MEMORY)
    } else {
        Found::pass()
    }
}

/// The two files `memory.write` owns, under the resolved workspace `root`.
fn is_memory_file(root: &Path, resolved: &Path) -> bool {
    resolved == root.join("USER.md") || resolved == root.join("memory/rules.md")
}

/// True when the word `w` (a path as the shell sees it, relative to `cwd`)
/// names one of the memory files. Cheap name test first: only a word ending
/// in one of the two file names is resolved.
fn names_memory_file(root: &Path, dirs: &[PathBuf], w: &str) -> bool {
    if !(w.ends_with("USER.md") || w.ends_with("rules.md")) {
        return false;
    }
    let p = PathBuf::from(w);
    if p.is_absolute() {
        return is_memory_file(root, &resolve(&p));
    }
    dirs.iter()
        .any(|d| is_memory_file(root, &resolve(&d.join(&p))))
}

/// The directories a relative path in `cmd` may be read from: the hook's cwd
/// and every `cd`/`pushd` target named in the command (one level deep).
fn command_dirs(cmd: &str, cwd: Option<&Path>) -> Vec<PathBuf> {
    let base = cwd.map_or_else(
        || std::env::current_dir().unwrap_or_default(),
        Path::to_path_buf,
    );
    let mut dirs = vec![base.clone()];
    for seg in tokenize(cmd) {
        if matches!(seg.first().map(String::as_str), Some("cd" | "pushd")) {
            if let Some(t) = seg[1..].iter().find(|x| !x.starts_with('-')) {
                dirs.push(base.join(t));
            }
        }
    }
    dirs
}

/// Targets of `>` and `>>` redirects in `cmd`. The tokenizer drops redirect
/// targets, so the raw text is read for them.
fn redirect_targets(cmd: &str) -> Vec<String> {
    static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let re = RE.get_or_init(|| {
        regex::Regex::new(r#"(?:>>?|>\|)\s*(?:'([^']*)'|"([^"]*)"|([^\s;|&<>()'"]+))"#)
            .expect("redirect regex")
    });
    re.captures_iter(cmd)
        .filter_map(|c| {
            (1..=3)
                .find_map(|i| c.get(i))
                .map(|m| m.as_str().to_string())
        })
        .collect()
}

/// A Bash command that writes one of the memory files: a redirect onto it, or
/// a command that is not read-only naming it (`sed -i`, `tee`, `cp`, a shell
/// `-c` string, …). A tripwire like the rest of the gate: a script that builds
/// the path at run time gets past it.
fn bash_memory(env: &Env, cmd: &str, cwd: Option<&Path>, depth: u8) -> Found {
    let Some(root) = env.runs_dir.parent() else {
        return Found::pass();
    };
    let root = resolve(root);
    let dirs = command_dirs(cmd, cwd);
    if redirect_targets(cmd)
        .iter()
        .any(|t| names_memory_file(&root, &dirs, t))
    {
        return Found::cos_only(ASK_MEMORY);
    }
    for seg in tokenize(cmd) {
        let Some(i) = seg.iter().position(|x| !keyword(x) && !is_assignment(x)) else {
            continue;
        };
        let base = seg[i].rsplit('/').next().unwrap_or(&seg[i]);
        if READ_ONLY.contains(&base) {
            continue;
        }
        for w in &seg[i + 1..] {
            if names_memory_file(&root, &dirs, w) {
                return Found::cos_only(ASK_MEMORY);
            }
            // A nested script (`bash -c '…'`, `eval '…'`).
            if depth < 3 && w.contains(char::is_whitespace) {
                let inner = bash_memory(env, w, cwd, depth + 1);
                if !inner.is_pass() {
                    return inner;
                }
            }
        }
    }
    Found::pass()
}

/// Lexical `.`/`..` removal, then symlinks resolved on the longest existing prefix.
fn resolve(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            c => out.push(c.as_os_str()),
        }
    }
    let mut tail = Vec::new();
    let mut head = out.clone();
    loop {
        if let Ok(c) = head.canonicalize() {
            return tail.iter().rev().fold(c, |acc, t| acc.join(t));
        }
        match (head.file_name().map(|f| f.to_owned()), head.parent()) {
            (Some(f), Some(par)) => {
                tail.push(f);
                head = par.to_path_buf();
            }
            _ => return out,
        }
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

/// Check one shell command with no daemon to ask (an approval-list hit denies).
pub fn check(command: &str) -> Verdict {
    judge(scan(command), &|| Caller::Unknown, None, None)
}

/// Every simple command in `command`, nested ones included.
fn scan(command: &str) -> Found {
    let segs = tokenize(command);
    let mut f = Found::pass();
    for seg in &segs {
        f.absorb(check_words(seg));
    }
    // A shell that reads its script from stdin (`| bash`, `bash -s`, a
    // here-string or heredoc) hides the script from the tokenizer: block when
    // the command text names a push.
    if mentions_push(command) && segs.iter().any(|s| reads_stdin_script(s)) {
        f.absorb(Found::block(PUSH_MESSAGE));
    }
    f
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

/// `herdr pane send-keys|send-text|run`: typing into another agent's pane.
fn input_pair(a: &str, b: &str) -> bool {
    a == "pane" && matches!(b, "send-keys" | "send-text" | "run")
}

fn check_words(w: &[String]) -> Found {
    let Some(i) = w.iter().position(|x| !keyword(x) && !is_assignment(x)) else {
        return Found::pass();
    };
    let base = w[i].rsplit('/').next().unwrap_or(&w[i]);
    let a = &w[i + 1..];
    match base {
        "herdr" => check_herdr(a),
        "git" => check_git(a),
        "gh" => check_gh(a),
        "sqlite3" => check_sqlite(a),
        b if is_shell(b) => check_shell(a),
        "busybox" if a.first().is_some_and(|x| is_shell(x)) => check_shell(&a[1..]),
        "eval" if !a.is_empty() => scan(&a.join(" ")),
        "find" => check_find(a),
        "sudo" | "env" | "command" | "exec" | "nice" | "time" | "nohup" | "setsid" | "stdbuf"
        | "xargs" | "builtin" | "chrt" | "ionice" | "taskset" | "timeout" => wrapper(base, a),
        b => names_db(b, a),
    }
}

/// Commands that only read their file arguments.
const READ_ONLY: &[&str] = &[
    "cat",
    "ls",
    "stat",
    "file",
    "head",
    "tail",
    "wc",
    "du",
    "less",
    "more",
    "md5sum",
    "sha1sum",
    "sha256sum",
    "shasum",
    "strings",
    "grep",
    "rg",
    "echo",
    "printf",
    "diff",
    "cmp",
    "test",
    "[",
    "realpath",
    "readlink",
    "basename",
    "dirname",
    "xxd",
    "hexdump",
    "od",
];

/// Any other command naming the daemon's `qq.db`, bar the read-only tools.
fn names_db(base: &str, a: &[String]) -> Found {
    let names = a.iter().any(|x| x.rsplit('/').next() == Some("qq.db"));
    if names && !READ_ONLY.contains(&base) {
        Found::act(Action::DbWrite, ASK_DB)
    } else {
        Found::pass()
    }
}

fn check_shell(a: &[String]) -> Found {
    for k in 0..a.len() {
        if is_c_flag(&a[k]) && k + 1 < a.len() {
            return scan(&a[k + 1]);
        }
    }
    Found::pass()
}

/// `find … -exec|-execdir|-ok|-okdir CMD … ;|+`: CMD is checked.
fn check_find(a: &[String]) -> Found {
    let mut f = Found::pass();
    for (k, w) in a.iter().enumerate() {
        if matches!(w.as_str(), "-exec" | "-execdir" | "-ok" | "-okdir") {
            let cmd: Vec<String> = a[k + 1..]
                .iter()
                .take_while(|x| x.as_str() != ";" && x.as_str() != "+")
                .cloned()
                .collect();
            f.absorb(check_words(&cmd));
        }
    }
    f
}

/// `sqlite3` on the daemon's `qq.db` without `-readonly`.
fn check_sqlite(a: &[String]) -> Found {
    let readonly = a.iter().any(|x| x == "-readonly" || x == "--readonly");
    // `.open` and `ATTACH` reopen a database read-write, past `-readonly`.
    let reopens = a.iter().any(|x| {
        let x = x.to_ascii_lowercase();
        x.contains(".open")
            || x.split(|c: char| !c.is_ascii_alphanumeric() && c != '_')
                .any(|t| t == "attach")
    });
    if (!readonly || reopens) && a.iter().any(|x| x.contains("qq.db")) {
        return Found::act(Action::DbWrite, ASK_DB);
    }
    Found::pass()
}

/// What `gh api`'s own flag parser (pflag) sees in the words after `api`.
struct ApiCall {
    /// Every real `-X`/`--method` value, upper-cased.
    methods: Vec<String>,
    /// A field or `--input` flag is present (gh then defaults to POST).
    has_fields: bool,
}

impl ApiCall {
    /// gh's method: the last explicit one, else POST with fields, else GET.
    fn effective(&self) -> &str {
        match self.methods.last() {
            Some(m) => m,
            None if self.has_fields => "POST",
            None => "GET",
        }
    }

    /// Fail closed: the effective method or any explicit one is not GET.
    fn writes(&self) -> bool {
        self.effective() != "GET" || self.methods.iter().any(|m| m != "GET")
    }

    fn explicit_get(&self) -> bool {
        !self.methods.is_empty() && self.methods.iter().all(|m| m == "GET")
    }
}

/// `gh api` value-taking flags, short and long: they consume the next word
/// (unless joined: `-Xv`, `-X=v`, `--flag=v`), so a word like `-XGET` after
/// `-p` or `--jq` is a value, not a method.
fn takes_value(short: Option<char>, long: &str) -> bool {
    short.is_some_and(|c| matches!(c, 'p' | 'q' | 't' | 'H' | 'f' | 'F' | 'X'))
        || matches!(
            long,
            "preview"
                | "jq"
                | "template"
                | "header"
                | "raw-field"
                | "field"
                | "input"
                | "hostname"
                | "cache"
                | "method"
        )
}

fn parse_gh_api(a: &[String]) -> ApiCall {
    let mut call = ApiCall {
        methods: Vec::new(),
        has_fields: false,
    };
    let mut k = a.iter().position(|x| x == "api").map_or(a.len(), |p| p + 1);
    while k < a.len() {
        let w = a[k].as_str();
        k += 1;
        if w == "--" {
            break;
        }
        // (flag name, joined value) for a flag that takes a value.
        let mut flag: Option<(String, Option<String>)> = None;
        if let Some(long) = w.strip_prefix("--") {
            let (name, joined) = match long.split_once('=') {
                Some((n, v)) => (n, Some(v.to_string())),
                None => (long, None),
            };
            if takes_value(None, name) {
                flag = Some((name.to_string(), joined));
            }
        } else if let Some(shorts) = w.strip_prefix('-').filter(|x| !x.is_empty()) {
            // A shorthand group: bool flags, then the first value-taking one.
            for (i, c) in shorts.char_indices() {
                if takes_value(Some(c), "") {
                    let rest = &shorts[i + c.len_utf8()..];
                    let rest = rest.strip_prefix('=').unwrap_or(rest);
                    flag = Some((c.to_string(), (!rest.is_empty()).then(|| rest.to_string())));
                    break;
                }
            }
        }
        let Some((name, joined)) = flag else {
            continue;
        };
        let value = match joined {
            Some(v) => Some(v),
            None => {
                k += 1;
                a.get(k - 1).cloned()
            }
        };
        match name.as_str() {
            "X" | "method" => call.methods.extend(value.map(|v| v.to_ascii_uppercase())),
            "f" | "F" | "field" | "raw-field" | "input" => call.has_fields = true,
            _ => {}
        }
    }
    call
}

/// `main` or `master`, bare or as `refs/heads/…`.
fn is_trunk(x: &str) -> bool {
    matches!(
        x.strip_prefix("refs/heads/").unwrap_or(x),
        "main" | "master"
    )
}

/// A word that targets main/master as a ref (`…/heads/main`, `ref=refs/heads/main`)
/// or a branch field (`branch=main`).
fn targets_trunk(x: &str) -> bool {
    let at_end = |rest: &str| {
        !rest.starts_with(|c: char| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.')
    };
    ["heads/main", "heads/master", "branch=main", "branch=master"]
        .iter()
        .any(|p| x.match_indices(p).any(|(i, _)| at_end(&x[i + p.len()..])))
}

/// `gh pr merge`, a merge endpoint through `gh api`, a merge mutation (hard
/// blocks); a ref/contents write or a hidden query is a push by another name:
/// blocked when it lands on main/master, else on the approval list.
fn check_gh(a: &[String]) -> Found {
    let pr = a.iter().position(|x| x == "pr");
    if pr.is_some_and(|p| a[p + 1..].iter().any(|x| x == "merge")) {
        return Found::block(MERGE_MESSAGE);
    }
    if a.windows(2).any(|w| w[0] == "release" && w[1] == "create") {
        return Found::act(Action::Release, ASK_RELEASE);
    }
    if a.iter().any(|x| x == "api") {
        let merge_path = a.iter().any(|x| {
            let path = x.split('?').next().unwrap_or(x).trim_end_matches('/');
            !x.starts_with('-') && (path.ends_with("/merge") || path.ends_with("/merges"))
        });
        let call = parse_gh_api(a);
        let explicit_get = call.explicit_get();
        if (merge_path && !explicit_get)
            || a.iter()
                .any(|x| x.contains("mergePullRequest") || x.contains("enablePullRequestAutoMerge"))
        {
            return Found::block(MERGE_MESSAGE);
        }
        if a.iter().any(|x| x.contains("query=@")) {
            return Found::act(Action::Bypass, ASK_BYPASS);
        }
        let writes_ref = a
            .iter()
            .any(|x| !x.starts_with('-') && (x.contains("/git/refs") || x.contains("/contents/")));
        if writes_ref && call.writes() {
            let contents = a
                .iter()
                .any(|x| !x.starts_with('-') && x.contains("/contents/"));
            // A contents write with no explicit branch lands on the default branch.
            let off_main = a.iter().any(|x| x.starts_with("branch="))
                || a.windows(2).any(|w| w[1].starts_with("branch="));
            if a.iter().any(|x| targets_trunk(x)) || (contents && !off_main) {
                return Found::block(PUSH_MESSAGE);
            }
            return Found::act(Action::Bypass, ASK_BYPASS);
        }
    }
    Found::pass()
}

/// A guarded pair counts only as the command's verb: every word before it is
/// a global option or that option's value, and no `--help` follows.
fn check_herdr(a: &[String]) -> Found {
    if matches!(a.first().map(String::as_str), Some("--help" | "-h")) {
        return Found::pass();
    }
    // `report-metadata … grid_role=…` forges the CoS identity (a pane token).
    if let Some(k) =
        (0..a.len().saturating_sub(1)).find(|&k| a[k] == "pane" && a[k + 1] == "report-metadata")
    {
        if a[k + 2..].iter().any(|x| x.contains("grid_role=")) {
            return Found::act(Action::PaneInput, ASK_INPUT);
        }
    }
    let Some(first) = (0..a.len().saturating_sub(1))
        .find(|&k| guarded_pair(&a[k], &a[k + 1]) || input_pair(&a[k], &a[k + 1]))
    else {
        return Found::pass();
    };
    let is_verb = (0..first).all(|k| a[k].starts_with('-') || (k > 0 && a[k - 1].starts_with('-')));
    let help = a.get(first + 2).is_some_and(|x| x == "--help" || x == "-h");
    if !is_verb || help {
        Found::pass()
    } else if input_pair(&a[first], &a[first + 1]) {
        Found::act(Action::PaneInput, ASK_INPUT)
    } else {
        Found::block(PANE_MESSAGE)
    }
}

/// `git [global options] <verb> …`: a push is blocked (a pure branch delete is
/// on the approval list), `--help` aside.
fn check_git(a: &[String]) -> Found {
    let mut i = 0;
    while i < a.len() {
        let w = a[i].as_str();
        if w == "-c" || (w.starts_with("-c") && w.len() > 2) {
            let cfg = if w == "-c" { a.get(i + 1) } else { Some(&a[i]) };
            let cfg = cfg.map(|c| c.trim_start_matches("-c")).unwrap_or("");
            if let Some(rest) = strip_alias(cfg) {
                let body = rest.split_once('=').map_or("", |x| x.1);
                if body
                    .split_whitespace()
                    .any(|t| t.trim_start_matches('!') == "push")
                {
                    return Found::act(Action::Bypass, ASK_BYPASS);
                }
            }
            i += if w == "-c" { 2 } else { 1 };
        } else if w == "-C" {
            i += 2;
        } else if w == "--help" || w == "-h" {
            return Found::pass();
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
    let rest = a.get(i + 1..).unwrap_or_default();
    match a.get(i).map(String::as_str) {
        Some("config") => check_git_config(rest),
        Some("branch") => check_git_branch(rest),
        Some("tag") => check_git_tag(rest),
        Some("filter-branch" | "filter-repo") => Found::act(Action::HistoryRewrite, ASK_REWRITE),
        Some("update-ref") => {
            let shared = rest.iter().any(|x| {
                x.starts_with("refs/remotes/")
                    || x.starts_with("refs/tags/")
                    || is_trunk(x) && x.starts_with("refs/")
            });
            if shared {
                Found::act(Action::HistoryRewrite, ASK_REWRITE)
            } else {
                Found::pass()
            }
        }
        Some("push") => {
            if rest.iter().any(|x| x == "--help" || x == "-h") {
                return Found::pass();
            }
            match push_deletes(rest) {
                Some(names) if names.iter().any(|n| is_trunk(n)) => Found::block(BRANCH_MESSAGE),
                Some(names) if !names.is_empty() => delete_hits(names),
                _ => Found::block(PUSH_MESSAGE),
            }
        }
        _ => Found::pass(),
    }
}

fn delete_hits(names: Vec<String>) -> Found {
    let mut f = Found::pass();
    for n in names {
        f.listed.push((Pending::BranchDelete(n), ASK_BRANCH_DELETE));
    }
    f
}

/// The branches a push deletes, when it only deletes: `--delete`/`-d`, or every
/// refspec is `:name`. `None` when it pushes anything.
fn push_deletes(a: &[String]) -> Option<Vec<String>> {
    let flag = a.iter().any(|x| x == "--delete" || x == "-d");
    let words: Vec<&str> = a
        .iter()
        .filter(|x| !x.starts_with('-'))
        .map(String::as_str)
        .collect();
    // The first word is the remote.
    let specs = words.get(1..).unwrap_or_default();
    if specs.is_empty() || (!flag && !specs.iter().all(|s| s.starts_with(':') && s.len() > 1)) {
        return None;
    }
    Some(
        specs
            .iter()
            .map(|s| s.trim_start_matches(':').to_string())
            .collect(),
    )
}

/// The part after `alias.`: git config keys are case-insensitive.
fn strip_alias(s: &str) -> Option<&str> {
    let (head, rest) = (s.get(..6)?, s.get(6..)?);
    head.eq_ignore_ascii_case("alias.").then_some(rest)
}

/// `git config … alias.<x> <value naming push>`: a push by another name.
fn check_git_config(a: &[String]) -> Found {
    let Some(k) = a.iter().position(|x| strip_alias(x).is_some()) else {
        return Found::pass();
    };
    let names_push = a[k + 1..].iter().any(|v| {
        v.split(|c: char| !c.is_ascii_alphanumeric() && c != '_' && c != '-')
            .any(|t| t == "push")
    });
    if names_push {
        Found::act(Action::Bypass, ASK_BYPASS)
    } else {
        Found::pass()
    }
}

/// `git branch`: deleting main/master blocks; deleting any other branch and
/// renaming or force-moving main/master are on the approval list.
fn check_git_branch(a: &[String]) -> Found {
    let short = |c: char| {
        a.iter()
            .any(|x| x.starts_with('-') && !x.starts_with("--") && x[1..].chars().any(|k| k == c))
    };
    let long = |n: &str| a.iter().any(|x| x == n);
    let names: Vec<&str> = a
        .iter()
        .filter(|x| !x.starts_with('-'))
        .map(String::as_str)
        .collect();
    if long("--delete") || short('d') || short('D') {
        if names.iter().any(|n| is_trunk(n)) {
            return Found::block(BRANCH_MESSAGE);
        }
        return delete_hits(names.iter().map(|n| n.to_string()).collect());
    }
    let moves = long("--move") || short('m') || short('M');
    let forces = long("--force") || short('f');
    if (moves && names.iter().any(|n| is_trunk(n)))
        || (forces && names.first().is_some_and(|n| is_trunk(n)))
    {
        return Found::act(Action::TrunkMove, ASK_TRUNK_MOVE);
    }
    Found::pass()
}

/// `git tag` that creates, moves or deletes a tag; listing and reading are free.
fn check_git_tag(a: &[String]) -> Found {
    let reads = a.iter().any(|x| {
        matches!(
            x.as_str(),
            "-l" | "--list"
                | "-v"
                | "--verify"
                | "--contains"
                | "--no-contains"
                | "--points-at"
                | "--merged"
                | "--no-merged"
        ) || x.starts_with("-n")
            || x.starts_with("--sort")
            || x.starts_with("--format")
    });
    let writes = a.iter().any(|x| {
        !x.starts_with('-')
            || matches!(
                x.as_str(),
                "-a" | "-s"
                    | "-f"
                    | "-d"
                    | "-m"
                    | "-u"
                    | "--delete"
                    | "--annotate"
                    | "--sign"
                    | "--force"
            )
    });
    if writes && !reads {
        Found::act(Action::Tag, ASK_TAG)
    } else {
        Found::pass()
    }
}

fn wrapper(base: &str, a: &[String]) -> Found {
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
                        let f = scan(p);
                        if !f.is_pass() {
                            return f;
                        }
                    }
                }
                k += 2;
                continue;
            }
        }
        if let Some(p) = a[k].strip_prefix("--split-string=") {
            let f = scan(p);
            if !f.is_pass() {
                return f;
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
    Found::pass()
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
    match evaluate_in(env, &json) {
        Verdict::Allow => {
            if let Some(ws) = env.runs_dir.parent() {
                if let Some(msg) = drift_warning(ws, &json) {
                    eprintln!("quaqua: {msg}");
                }
            }
            Ok(String::new())
        }
        Verdict::Ask(msg) => Ok(ask_json(msg)),
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
