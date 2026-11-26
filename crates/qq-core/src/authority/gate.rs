//! The `gate` verb: the Claude Code PreToolUse hook (Bash, and Write/Edit on
//! the live instruction files). Stateless: it tokenizes the hook's command and
//! sorts what it finds in three bins.
//!
//! - Hard blocks, for everyone: a herdr pane, tab or workspace spawn/move/close,
//!   any `git push` that is not a pure branch delete, a `gh` merge that
//!   bypasses the owner or is not the whole readable command (`--admin`,
//!   `--auto`, `--disable-auto`, a `gh api` merge endpoint or mutation, a merge
//!   flag the scanner does not know, a word the tokenizer drops or bash
//!   expands, globs or brace-expands, a merge behind a wrapper, `xargs`,
//!   `parallel` or `find -exec`), deleting main/master, and a ref or contents
//!   write that lands on main/master.
//! - The approval list, decided by caller and tier (R7, R14, R5): branch delete,
//!   `git branch -m|-M|-f` on main/master, `git tag`, `gh release create`,
//!   `gh pr merge` (the owner's prompt at every tier, lane and grant; only the
//!   daemon's `pr.merge` door keeps the tier math), history
//!   rewrite on shared refs, the #297 bypass spellings (alias push,
//!   `gh api` ref/contents writes off main, `query=@file`), pane input and
//!   `grid_role` tokens, Bash naming `runs/qq.db`, and Write/Edit of the live
//!   AGENTS.md and identity.md. Only a hit asks the daemon who is
//!   calling (`caller.lookup`); only the CoS gets past `deny`, and the tier
//!   table (`tier.rs`) says allow or ask. No daemon answer is `deny`. A branch
//!   delete is classed live against the remote default: merged is low, unmerged
//!   is high and asks at every tier, a full grant for the repo the only exception.
//! - The two memory files (R4), `USER.md` and `memory/rules.md` of the live
//!   workspace: a Write/Edit to either, or a Bash command that writes one (a
//!   redirect onto it, or a non-read-only command naming it), is `deny` for
//!   every caller, the CoS included (its message names the cos-memory tools);
//!   no daemon answer is `deny` too. The one writer is the daemon verb
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
const COS_MEMORY_MESSAGE: &str = "blocked: USER.md and memory/rules.md are written only through the cos-memory tools (memory_add, memory_replace, memory_remove), never directly";
const TIER_DENY_MESSAGE: &str = "blocked: pushing to main and force-pushing are anh iu's call";

const ASK_BRANCH_DELETE: &str = "approval list: delete a branch";
const ASK_TRUNK_MOVE: &str = "approval list: rename or force-move main/master";
const ASK_TAG: &str = "approval list: git tag";
const ASK_RELEASE: &str = "approval list: gh release create";
const ASK_MERGE: &str = "approval list: gh pr merge";
const ASK_REWRITE: &str = "approval list: history rewrite on shared refs";
const ASK_BYPASS: &str = "approval list: a push or merge door by another name";
const ASK_INPUT: &str = "approval list: pane input or a grid_role token";
const ASK_DB: &str = "approval list: runs/qq.db belongs to the daemon";
const ASK_FILE: &str = "approval list: the CoS's live instruction files";
const ASK_GRANT: &str = "approval list: a grant, tier or lane write needs anh iu's consent";
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
/// `BranchDelete` carries the remote of a push delete (None: a local branch).
#[derive(Debug)]
enum Pending {
    Act(Action),
    BranchDelete(String, Option<String>),
    /// Only the CoS may do it, and the CoS needs no approval for it: no tier.
    CosOnly,
}

/// The repo a command acts on, as far as the scanner can tell: the hook's cwd,
/// a literal `git -C <dir>` path, or unknown (`cd`, `--git-dir`, `GIT_DIR=`,
/// `gh -R`, a path with a variable): unknown is judged at `Tier::High`.
#[derive(Debug, Clone, Default, PartialEq)]
enum Target {
    #[default]
    Here,
    Dir(String),
    Unknown,
}

#[derive(Debug)]
struct Hit {
    p: Pending,
    reason: &'static str,
    target: Target,
}

/// A path a Bash command writes, resolved against the CoS workspace later.
/// `loose`: a `cd` the scanner could not follow came first, so a protected
/// basename counts wherever it sits.
#[derive(Debug)]
struct PathHit {
    path: String,
    loose: bool,
}

/// What a scan found: the first hard block, every approval-list hit and every
/// path a Bash command writes.
#[derive(Debug, Default)]
struct Found {
    block: Option<&'static str>,
    listed: Vec<Hit>,
    paths: Vec<PathHit>,
    /// A `cd`-like word, a `GIT_DIR=`-style assignment or a chdir flag was seen
    /// (in a nested script too): the command may act on another directory.
    moved: bool,
}

impl Found {
    fn pass() -> Self {
        Self::default()
    }

    fn block(m: &'static str) -> Self {
        Found {
            block: Some(m),
            ..Self::default()
        }
    }

    fn act(a: Action, reason: &'static str) -> Self {
        let mut f = Self::default();
        f.listed.push(Hit {
            p: Pending::Act(a),
            reason,
            target: Target::Here,
        });
        f
    }

    fn cos_only(reason: &'static str) -> Self {
        let mut f = Self::default();
        f.listed.push(Hit {
            p: Pending::CosOnly,
            reason,
            target: Target::Here,
        });
        f
    }

    fn is_pass(&self) -> bool {
        self.block.is_none() && self.listed.is_empty() && self.paths.is_empty()
    }

    fn absorb(&mut self, o: Found) {
        self.block = self.block.or(o.block);
        self.listed.extend(o.listed);
        self.paths.extend(o.paths);
        self.moved |= o.moved;
    }

    /// Point every hit at `t`, a `Dir` one included.
    fn force_target(mut self, t: &Target) -> Self {
        for h in &mut self.listed {
            h.target = t.clone();
        }
        self
    }

    /// Point every hit that still acts on the cwd at `t`.
    fn retarget(mut self, t: &Target) -> Self {
        if *t != Target::Here {
            for h in &mut self.listed {
                if h.target == Target::Here {
                    h.target = t.clone();
                }
            }
        }
        self
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
                (Some(p), Some(env)) => edit_found(env, p, cwd.as_deref(), false),
                // No workspace to resolve against: treat as a hit and deny.
                (Some(_), None) => Found::act(Action::InstructionFile, ASK_FILE),
            }
        }
        (_, None | Some("Bash")) => return Verdict::Block(PANE_MESSAGE),
        _ => return Verdict::Allow,
    };
    let mut found = found;
    for h in std::mem::take(&mut found.paths) {
        found.absorb(match env {
            Some(env) => edit_found(env, &h.path, cwd.as_deref(), h.loose),
            // No workspace to resolve against: a protected basename is a hit.
            None => match protected_kind(&h.path) {
                Some(_) => Found::act(Action::InstructionFile, ASK_FILE),
                None => Found::pass(),
            },
        });
    }
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
    let memory = f.listed.iter().any(|h| matches!(h.p, Pending::CosOnly));
    match who() {
        Caller::Cos if memory => return Verdict::Block(COS_MEMORY_MESSAGE),
        Caller::Cos => {}
        Caller::Other(_) if memory => return Verdict::Block(MEMORY_MESSAGE),
        Caller::Other(_) => return Verdict::Block(NOT_COS_MESSAGE),
        Caller::Unknown => return Verdict::Block(NO_CALLER_MESSAGE),
    }
    let here = cwd.unwrap_or(Path::new("."));
    let mut ask = None;
    for h in f.listed {
        // The tier is the target repo's, not the hook cwd's; no target, no
        // runs dir or an unreadable path is high.
        let (dir, ctx, known) = match (&h.target, runs_dir) {
            (Target::Here, Some(r)) => (
                here.to_path_buf(),
                tier::effective_context_tier(r, here, true),
                true,
            ),
            (Target::Dir(d), Some(r)) => match target_dir(d, here) {
                Some(p) => {
                    let c = tier::effective_context_tier(r, &p, true);
                    (p, c, true)
                }
                None => (here.to_path_buf(), Tier::High, false),
            },
            _ => (here.to_path_buf(), Tier::High, false),
        };
        let action = match h.p {
            Pending::Act(a) => a,
            Pending::BranchDelete(b, remote) => {
                if h.target == Target::Unknown {
                    Action::BranchDeleteUnmerged
                } else {
                    tier::branch_delete_class(&dir, &b, remote.as_deref())
                }
            }
            Pending::CosOnly => continue,
        };
        // A merge is the owner's call at every tier, lane and grant; only the
        // daemon's `pr.merge` door keeps the tier math.
        if action == Action::MergeRequest {
            ask = Some(h.reason);
            continue;
        }
        // A full grant is the one exception to "unmerged asks at every tier",
        // and only for a target the scanner could read.
        if action == Action::BranchDeleteUnmerged
            && known
            && runs_dir.is_some_and(|r| tier::grant_full(r, &dir))
        {
            continue;
        }
        match tier::decide(true, action, ctx, ctx) {
            Decision::Deny => return Verdict::Block(TIER_DENY_MESSAGE),
            Decision::Ask => ask = Some(h.reason),
            Decision::Allow => {}
        }
    }
    ask.map_or(Verdict::Allow, Verdict::Ask)
}

/// `~`, `$HOME` and `${HOME}` at the start of a path.
fn expand_home(p: &str) -> String {
    let home = std::env::var("HOME").unwrap_or_default();
    match p {
        "~" | "$HOME" | "${HOME}" => home,
        _ => match p
            .strip_prefix("~/")
            .or_else(|| p.strip_prefix("$HOME/"))
            .or_else(|| p.strip_prefix("${HOME}/"))
        {
            Some(rest) if !home.is_empty() => format!("{home}/{rest}"),
            _ => p.to_string(),
        },
    }
}

/// A literal `-C` directory as a path, or None when it holds a variable, a
/// glob or a substitution.
fn target_dir(d: &str, here: &Path) -> Option<PathBuf> {
    let d = expand_home(d);
    if d.is_empty() || d.contains(['$', '`', '*', '?', '~']) {
        return None;
    }
    let p = PathBuf::from(d);
    Some(if p.is_relative() { here.join(p) } else { p })
}

/// Which protected workspace file a basename names (the live instruction files).
fn protected_kind(file: &str) -> Option<Action> {
    match file.rsplit('/').next().unwrap_or(file) {
        "AGENTS.md" | "identity.md" => Some(Action::InstructionFile),
        _ => None,
    }
}

/// A Write/Edit to the live AGENTS.md or identity.md: the path is
/// resolved (cwd, `..`, symlinks) and compared under the CoS workspace root, so
/// a template or a repo AGENTS.md with the same basename is not a hit.
fn edit_found(env: &Env, file: &str, cwd: Option<&Path>, loose: bool) -> Found {
    let Some(root) = env.runs_dir.parent() else {
        return Found::act(Action::InstructionFile, ASK_FILE);
    };
    let root = resolve(root);
    let file = expand_home(file);
    let mut p = PathBuf::from(&file);
    if p.is_relative() {
        p = cwd
            .map_or_else(
                || std::env::current_dir().unwrap_or_default(),
                Path::to_path_buf,
            )
            .join(p);
    }
    let p = resolve(&p);
    if ["AGENTS.md", "identity.md"]
        .iter()
        .any(|n| p == root.join(n))
    {
        Found::act(Action::InstructionFile, ASK_FILE)
    } else if is_memory_file(&root, &p) {
        Found::cos_only(ASK_MEMORY)
    } else if loose {
        match protected_kind(&file) {
            Some(a) => Found::act(a, ASK_FILE),
            None => Found::pass(),
        }
    } else {
        Found::pass()
    }
}

/// The two files `memory.write` owns, under the resolved workspace `root`:
/// the one list is [`crate::memory::TARGETS`], which `memory/fence.rs` reads too.
fn is_memory_file(root: &Path, resolved: &Path) -> bool {
    crate::memory::TARGETS
        .iter()
        .any(|t| resolved == root.join(t.rel))
}

/// Which live file a Bash word names: a memory file (denied to everyone) or an
/// instruction file (the CoS asks, the rest are denied).
#[derive(Clone, Copy, PartialEq)]
enum Live {
    Memory,
    Instruction,
}

fn live_kind(root: &Path, resolved: &Path) -> Option<Live> {
    if ["AGENTS.md", "identity.md"]
        .iter()
        .any(|n| resolved == root.join(n))
    {
        Some(Live::Instruction)
    } else if is_memory_file(root, resolved) {
        Some(Live::Memory)
    } else {
        None
    }
}

/// What the word `w` (a path as the shell sees it, relative to `cwd`) names
/// among the live memory and instruction files, or is `key=<path>` (`dd of=`,
/// `--output=`). A leading `~`, `$HOME`, `${HOME}` is expanded; `$PWD` is each
/// `dirs` entry. Cheap name test first: only a word ending in one of the five
/// file names is resolved. A memory hit wins over an instruction hit.
fn names_live_file(root: &Path, dirs: &[PathBuf], w: &str) -> Option<Live> {
    const NAMES: [&str; 4] = ["USER.md", "rules.md", "AGENTS.md", "identity.md"];
    if !NAMES.iter().any(|n| w.ends_with(n)) {
        return None;
    }
    let value = match w.split_once('=') {
        Some((k, v))
            if !k.is_empty()
                && k.chars()
                    .all(|c| c.is_ascii_alphanumeric() || "-_".contains(c)) =>
        {
            Some(v)
        }
        _ => None,
    };
    let kinds = [Some(w), value]
        .into_iter()
        .flatten()
        .filter_map(|w| names_one(root, dirs, w));
    kinds.fold(None, |acc, k| match (acc, k) {
        (Some(Live::Memory), _) | (_, Live::Memory) => Some(Live::Memory),
        _ => Some(Live::Instruction),
    })
}

fn names_one(root: &Path, dirs: &[PathBuf], w: &str) -> Option<Live> {
    let kind = |p: &Path| live_kind(root, &resolve(p));
    if let Some(rest) = w
        .strip_prefix("$PWD/")
        .or_else(|| w.strip_prefix("${PWD}/"))
    {
        return dirs
            .iter()
            .filter_map(|d| kind(&d.join(rest)))
            .max_by_key(|k| *k == Live::Memory);
    }
    let p = PathBuf::from(expand_home(w));
    if p.is_absolute() {
        return kind(&p);
    }
    dirs.iter()
        .filter_map(|d| kind(&d.join(&p)))
        .max_by_key(|k| *k == Live::Memory)
}

/// The directories a relative path in `cmd` may be read from: the hook's cwd,
/// every `cd`/`pushd` target named in the command (one level deep) and every
/// `-C`, `-D` or `--chdir` directory of any command (`git -C d`, `env -C d`).
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
        let mut cur = base.clone();
        for (k, w) in seg.iter().enumerate() {
            let d = match w.as_str() {
                "-C" | "-D" | "--chdir" => seg.get(k + 1).map(String::as_str),
                x => x.strip_prefix("--chdir="),
            };
            if let Some(d) = d.filter(|d| !d.contains(['$', '`'])) {
                cur = cur.join(expand_home(d));
                dirs.push(cur.clone());
            }
        }
    }
    dirs
}

/// Commands whose last plain word is the file they write; the earlier words are
/// only read.
fn copy_like(base: &str) -> bool {
    matches!(base, "cp" | "rsync" | "install" | "ln" | "scp")
}

/// A Bash command that writes one of the memory or instruction files: a
/// redirect onto it, or a command that is not read-only naming it (`sed -i`,
/// `tee`, `cp`, `perl -pi`, a shell `-c` string, …). A tripwire like the rest of
/// the gate: a script that builds the path at run time gets past it. A memory
/// hit is denied to everyone; an instruction hit asks the CoS. The source of a
/// copy (`cp AGENTS.md /tmp/x`) is a read.
fn bash_memory(env: &Env, cmd: &str, cwd: Option<&Path>, depth: u8) -> Found {
    let Some(root) = env.runs_dir.parent() else {
        return Found::pass();
    };
    let root = resolve(root);
    let dirs = command_dirs(cmd, cwd);
    let mut instruction = false;
    for t in redirect_targets(cmd) {
        match names_live_file(&root, &dirs, &t) {
            Some(Live::Memory) => return Found::cos_only(ASK_MEMORY),
            Some(Live::Instruction) => instruction = true,
            None => {}
        }
    }
    for seg in tokenize(cmd) {
        let Some(i) = seg.iter().position(|x| !keyword(x) && !is_assignment(x)) else {
            continue;
        };
        let base = seg[i].rsplit('/').next().unwrap_or(&seg[i]);
        if read_only_cmd(base, &seg[i + 1..]) {
            continue;
        }
        let args = &seg[i + 1..];
        let last_plain = args.iter().rposition(|x| !x.starts_with('-'));
        // An interpreter or awk program names its target inside a quoted
        // string (`open('USER.md','a')`): test the pieces between the quotes.
        let embedded = is_interp(base) || matches!(base, "awk" | "gawk" | "mawk");
        for (j, w) in args.iter().enumerate() {
            let mut kind = names_live_file(&root, &dirs, w);
            // `-oFILE`, `-ruoFILE`: the value is glued to the flag cluster.
            if kind.is_none() && w.starts_with('-') && !w.starts_with("--") {
                kind = w
                    .char_indices()
                    .skip(2)
                    .filter_map(|(k, _)| names_live_file(&root, &dirs, &w[k..]))
                    .fold(None, |acc, k| match (acc, k) {
                        (Some(Live::Memory), _) | (_, Live::Memory) => Some(Live::Memory),
                        _ => Some(Live::Instruction),
                    });
            }
            if embedded && kind != Some(Live::Memory) {
                for p in w.split(|c: char| c.is_whitespace() || "'\"(),;{}[]".contains(c)) {
                    match names_live_file(&root, &dirs, p) {
                        Some(Live::Memory) => kind = Some(Live::Memory),
                        Some(Live::Instruction) => kind = kind.or(Some(Live::Instruction)),
                        None => {}
                    }
                }
            }
            let dest = Some(j) == last_plain
                || (j > 0 && args[j - 1] == "-t")
                || w.starts_with("--target-directory=");
            match kind {
                Some(Live::Memory) => return Found::cos_only(ASK_MEMORY),
                Some(Live::Instruction) if !copy_like(base) || dest => instruction = true,
                _ => {}
            }
            // A nested script (`bash -c '…'`, `eval '…'`).
            if depth < 3 && w.contains(char::is_whitespace) {
                let inner = bash_memory(env, w, cwd, depth + 1);
                if inner.listed.iter().any(|h| matches!(h.p, Pending::CosOnly)) {
                    return inner;
                }
                instruction |= !inner.is_pass();
            }
        }
    }
    if instruction {
        Found::act(Action::InstructionFile, ASK_FILE)
    } else {
        Found::pass()
    }
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
    scan_with(command, true)
}

/// [`scan`]. `deep` is false for a quoted word that merely might be a command
/// line (an argument of an unlisted command): its command word must itself be a
/// known command, so prose that mentions `git push` is not read as one.
fn scan_with(command: &str, deep: bool) -> Found {
    let segs = tokenize(command);
    let mut f = Found::pass();
    // A second tokenization marks every word the first one drops (`$(…)`, a
    // backtick, `<(…)`, `''`/`""`). A merge segment that starts away from gh
    // (a wrapper, an assignment) or carries a mark has an argv bash builds and
    // the gate never reads: hard-block it for everyone, the CoS included.
    for seg in &tokenize(&mark_drops(command)) {
        if let Some(j) = gh_merge_in(seg) {
            if j != 0 || seg.iter().any(|w| w.contains(DROP)) {
                f.absorb(Found::block(MERGE_MESSAGE));
            }
        }
    }
    for seg in &segs {
        f.absorb(check_words_in(seg, deep));
        f.absorb(git_env_words(seg));
    }
    // Redirect targets are dropped by the tokenizer: read them off the text.
    for t in redirect_targets(command) {
        if t.rsplit('/').next() == Some("qq.db") {
            f.absorb(Found::act(Action::DbWrite, ASK_DB));
        }
        f.paths.push(PathHit {
            path: t,
            loose: false,
        });
    }
    // A raw RPC door naming a grant verb: a socket client with the verb anywhere
    // in the command text (it may arrive on a pipe), or an interpreter with
    // inline code that names it. `quaqua call` is read at its verb position.
    let lower = command.to_ascii_lowercase();
    let names_verb = |t: &str| GRANT_VERBS.iter().any(|v| t.contains(v));
    if segs.iter().any(|s| {
        let Some(i) = s.iter().position(|x| !keyword(x) && !is_assignment(x)) else {
            return false;
        };
        let base = s[i].rsplit('/').next().unwrap_or(&s[i]);
        match base {
            "socat" | "nc" | "ncat" | "netcat" | "curl" => names_verb(&lower),
            // Inline code names the verb in its own words; a script fed on stdin
            // (`python3 -`, a heredoc, a pipe) can name it anywhere.
            b if is_interp(b) => {
                let args = &s[i + 1..];
                args.iter().all(|x| x.starts_with('-')) && names_verb(&lower)
                    || args.iter().any(|x| inline_code_flag(x))
                        && args.iter().any(|x| names_verb(&x.to_ascii_lowercase()))
            }
            _ => false,
        }
    }) {
        f.absorb(Found::act(Action::GrantWrite, ASK_GRANT));
    }
    // `$(which quaqua) call grant.full`: the tokenizer drops the substituted
    // command word, so `call <grant verb>` counts at any segment.
    if segs.iter().any(|s| {
        s.windows(2)
            .any(|w| w[0] == "call" && GRANT_VERBS.contains(&w[1].to_ascii_lowercase().as_str()))
    }) {
        f.absorb(Found::act(Action::GrantWrite, ASK_GRANT));
    }
    // Anything that may change the directory or hide the command word (`cd`,
    // `builtin cd`, `eval cd`, `$X`, `GIT_DIR=`) makes every hit unknown, which
    // is judged at high. A literal `cd` target still reads relative write paths.
    let mut cd: Option<Option<String>> = f.moved.then_some(None);
    for seg in &segs {
        let Some(i) = seg.iter().position(|x| !keyword(x) && !is_assignment(x)) else {
            if seg.iter().any(|x| retargets(x)) {
                cd = Some(None);
            }
            continue;
        };
        let base = seg[i].rsplit('/').next().unwrap_or(&seg[i]);
        if matches!(base, "cd" | "pushd" | "popd" | "chdir") {
            let arg = seg[i + 1..].iter().find(|x| !x.starts_with('-'));
            cd = Some(match (base, arg) {
                ("cd" | "pushd" | "chdir", Some(a)) if !a.contains(['$', '`', '*', '?']) => {
                    Some(expand_home(a))
                }
                _ => None,
            });
        }
        // A cd-like word that is not the command word itself, a retargeting
        // assignment or a command word built at run time.
        if seg[i + 1..].iter().any(|x| cd_word(x))
            || seg.iter().any(|x| retargets(x))
            || base.contains(['$', '`'])
        {
            cd = Some(None);
        }
    }
    if cd.is_some() {
        f.moved = true;
        f = f.force_target(&Target::Unknown);
    }
    if let Some(dir) = cd {
        for h in &mut f.paths {
            match &dir {
                Some(d) if !h.path.starts_with('/') && !h.path.starts_with('~') => {
                    h.path = format!("{d}/{}", h.path);
                }
                None => h.loose = true,
                _ => {}
            }
        }
    }
    // A shell that reads its script from stdin (`| bash`, `bash -s`, a
    // here-string or heredoc) hides the script from the tokenizer: block when
    // the command text names a push.
    if mentions_push(command) && segs.iter().any(|s| reads_stdin_script(s)) {
        f.absorb(Found::block(PUSH_MESSAGE));
    }
    f
}

const GRANT_VERBS: [&str; 3] = ["grant.full", "grant.tier", "grant.lane"];

/// `cd`, `pushd`, `popd` or `chdir`, however the word is spelled as a path.
fn cd_word(w: &str) -> bool {
    matches!(
        w.rsplit('/').next().unwrap_or(w),
        "cd" | "pushd" | "popd" | "chdir"
    )
}

/// An interpreter flag that carries the program text (`-c`, `-e`, `-r`, `--eval`).
fn inline_code_flag(w: &str) -> bool {
    matches!(w, "--eval" | "--print" | "--exec")
        || (w.starts_with('-')
            && !w.starts_with("--")
            && w[1..]
                .chars()
                .any(|c| matches!(c, 'c' | 'e' | 'E' | 'r' | 'p')))
}

fn is_interp(base: &str) -> bool {
    [
        "python", "node", "ruby", "perl", "php", "deno", "bun", "lua",
    ]
    .iter()
    .any(|p| base.starts_with(p))
}

/// The words after each unquoted `>` redirect (`>`, `>>`, `>|`, `&>`, `N>`),
/// quotes stripped; fd duplications (`>&2`, `>&-`) and `>(…)` are not files.
fn redirect_targets(cmd: &str) -> Vec<String> {
    let c: Vec<char> = cmd.chars().collect();
    let mut out = Vec::new();
    let (mut i, mut q) = (0, '\0');
    while i < c.len() {
        let ch = c[i];
        i += 1;
        if q != '\0' {
            if ch == q {
                q = '\0';
            } else if ch == '\\' && q == '"' {
                i += 1;
            }
            continue;
        }
        match ch {
            '\'' | '"' => q = ch,
            '\\' => i += 1,
            '>' => {
                while i < c.len() && matches!(c[i], '>' | '|') {
                    i += 1;
                }
                if i < c.len() && c[i] == '&' {
                    if c.get(i + 1)
                        .is_some_and(|d| d.is_ascii_digit() || *d == '-')
                    {
                        i += 2;
                        continue;
                    }
                    i += 1;
                }
                while i < c.len() && c[i].is_whitespace() {
                    i += 1;
                }
                if c.get(i) == Some(&'(') {
                    continue;
                }
                let mut w = String::new();
                let mut wq = '\0';
                while i < c.len() {
                    let d = c[i];
                    if wq != '\0' {
                        if d == wq {
                            wq = '\0';
                        } else {
                            w.push(d);
                        }
                    } else if d == '\'' || d == '"' {
                        wq = d;
                    } else if d == '\\' && i + 1 < c.len() {
                        i += 1;
                        w.push(c[i]);
                    } else if d.is_whitespace() || ";|&<>()".contains(d) {
                        break;
                    } else {
                        w.push(d);
                    }
                    i += 1;
                }
                if !w.is_empty() {
                    out.push(w);
                }
            }
            _ => {}
        }
    }
    out
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

/// `GIT_DIR=…`, `GH_REPO=…` and friends retarget the command that follows.
fn retargets(w: &str) -> bool {
    ["GIT_DIR=", "GIT_WORK_TREE=", "GIT_COMMON_DIR=", "GH_REPO="]
        .iter()
        .any(|p| w.starts_with(p))
}

fn check_words(w: &[String]) -> Found {
    check_words_in(w, true)
}

fn check_words_in(w: &[String], deep: bool) -> Found {
    let Some(i) = w.iter().position(|x| !keyword(x) && !is_assignment(x)) else {
        return Found::pass();
    };
    let t = if w[..i].iter().any(|x| retargets(x)) {
        Target::Unknown
    } else {
        Target::Here
    };
    check_cmd(w, i, deep).retarget(&t)
}

fn check_cmd(w: &[String], i: usize, deep: bool) -> Found {
    let base = w[i].rsplit('/').next().unwrap_or(&w[i]);
    let a = &w[i + 1..];
    match base {
        "herdr" => check_herdr(a),
        "git" => check_git(a, deep),
        "gh" => check_gh(a),
        "quaqua" => {
            let mut f = check_quaqua(a);
            f.absorb(names_db(base, a));
            f
        }
        "sqlite3" => check_sqlite(a),
        b if is_shell(b) => check_shell(a),
        "busybox" if a.first().is_some_and(|x| is_shell(x)) => check_shell(&a[1..]),
        "eval" if !a.is_empty() => scan(&a.join(" ")),
        "find" => check_find(a),
        // The command word was a substitution the tokenizer dropped.
        "call" => check_quaqua(&w[i..]),
        // `xargs` and `parallel` add words from outside the text: a merge under
        // one cannot be read, so it stays a hard block.
        b @ ("xargs" | "parallel") => merge_behind(b, a),
        "sudo" | "env" | "command" | "exec" | "nice" | "time" | "nohup" | "setsid" | "stdbuf"
        | "builtin" | "chrt" | "ionice" | "taskset" | "timeout" => wrapper(base, a),
        b => {
            let mut f = names_db(b, a);
            // A command word built at run time (`$Q call …`) may be `quaqua`.
            if b.contains(['$', '`']) {
                f.absorb(check_quaqua(a));
            }
            // Not a provable reader and not a wrapper the gate has a table for:
            // it may run any of its words as a command (`caffeinate`, `doas`,
            // `watch`, `ssh host …`). Check them all, at an unknown target.
            if deep && !read_only_cmd(b, a) {
                f.absorb(scan_suffixes(a, 0));
                // The command word may itself be a command line (`sudo -s 'git push'`).
                let mut g = spaced_word(&w[i]);
                g.moved |= !g.is_pass();
                f.absorb(g.force_target(&Target::Unknown));
            }
            f.paths.extend(
                write_targets(b, a)
                    .into_iter()
                    .map(|path| PathHit { path, loose: false }),
            );
            f
        }
    }
}

/// `quaqua … call grant.full|tier|lane`: anh iu's consent, never silent. Only
/// the verb position counts: a word after `call` that is a grant verb, or a verb
/// the scanner cannot read (missing, `"$V"`, a `{}` placeholder).
fn check_quaqua(a: &[String]) -> Found {
    let Some(c) = a.iter().position(|x| x == "call") else {
        return Found::pass();
    };
    let after = &a[c + 1..];
    let readable = |x: &String| {
        !x.is_empty()
            && x.chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
    };
    let opaque = after
        .iter()
        .find(|x| !x.starts_with('-'))
        .is_none_or(|x| !readable(x));
    let names_grant = after
        .iter()
        .any(|x| GRANT_VERBS.contains(&x.to_ascii_lowercase().as_str()));
    if names_grant || opaque {
        Found::act(Action::GrantWrite, ASK_GRANT)
    } else {
        Found::pass()
    }
}

/// The paths a file-writing tool changes, as far as its words say.
fn write_targets(base: &str, a: &[String]) -> Vec<String> {
    let plain = || a.iter().filter(|x| !x.starts_with('-')).cloned();
    let in_place = |x: &String| {
        x == "--in-place"
            || x.starts_with("--in-place=")
            || (x.starts_with('-') && !x.starts_with("--") && x.contains(['i', 'I']))
    };
    match base {
        "tee" | "mv" | "truncate" | "rm" | "unlink" | "shred" => plain().collect(),
        "sed" if a.iter().any(in_place) => plain().collect(),
        "cp" | "install" => {
            let mut v: Vec<String> = plain().next_back().into_iter().collect();
            v.extend(a.windows(2).filter(|w| w[0] == "-t").map(|w| w[1].clone()));
            v.extend(
                a.iter()
                    .filter_map(|x| x.strip_prefix("--target-directory="))
                    .map(String::from),
            );
            v
        }
        "dd" => a
            .iter()
            .filter_map(|x| x.strip_prefix("of=").map(String::from))
            .collect(),
        _ => Vec::new(),
    }
}

/// Commands that only read their file arguments, whatever flags they get: none
/// of them has an output file, a program or a pager option in any
/// implementation. Everything else that reads must pass [`read_only_cmd`]'s
/// exact flag tables.
const READ_ONLY: &[&str] = &[
    "cat",
    "ls",
    "stat",
    "head",
    "tail",
    "wc",
    "du",
    "md5sum",
    "sha1sum",
    "sha256sum",
    "shasum",
    "strings",
    "grep",
    "echo",
    "printf",
    "cmp",
    "test",
    "[",
    "realpath",
    "readlink",
    "basename",
    "dirname",
    "hexdump",
    "od",
];

/// Read-only git subcommands.
const GIT_READ: &[&str] = &[
    "diff",
    "log",
    "show",
    "blame",
    "grep",
    "status",
    "ls-files",
    "cat-file",
    "shortlog",
    "rev-parse",
    "reflog",
    "describe",
];

/// True when `base args…` only reads its file arguments: the [`READ_ONLY`]
/// list, plus the tools that read unless a flag or program makes them write
/// (`sed -i`, `sort -o`, awk `print >`, `git diff --output=`).
fn read_only_cmd(base: &str, args: &[String]) -> bool {
    match base {
        _ if READ_ONLY.contains(&base) => true,
        "jq" | "nl" | "cut" | "tac" => true,
        "uniq" => uniq_reads(args),
        "sed" => sed_reads(args),
        "awk" | "gawk" | "mawk" => awk_reads(args),
        // Exact tables: a flag or letter that is not listed is not a read.
        "sort" => flags_ok(
            args,
            "bcCdfghiMmnrRsuVz",
            "kStT",
            "reverse numeric-sort general-numeric-sort human-numeric-sort ignore-case \
             ignore-leading-blanks ignore-nonprinting dictionary-order unique stable merge \
             check month-sort random-sort version-sort zero-terminated",
            "key field-separator buffer-size temporary-directory parallel batch-size",
        ),
        "rg" => flags_ok(
            args,
            "aiInNsSvwxcHlLoqUuFPpVb0.",
            "efgmtTABCMjdrE",
            "hidden no-ignore no-ignore-vcs no-ignore-parent files files-with-matches \
             files-without-match count count-matches line-number no-line-number ignore-case \
             smart-case case-sensitive fixed-strings word-regexp line-regexp invert-match \
             only-matching quiet no-heading heading with-filename no-filename json column \
             no-messages follow text multiline unrestricted pcre2 stats vimgrep trim null \
             sort-files binary no-binary no-config",
            "glob iglob type type-not max-depth max-count max-filesize context after-context \
             before-context color regexp file replace threads encoding sort sortr max-columns",
        ),
        "bat" | "batcat" => flags_ok(
            args,
            "nApPufds",
            "lrHm",
            "plain number show-all diff no-config unbuffered force-colorization squeeze-blank",
            "language style color theme line-range highlight-line paging decorations wrap tabs \
             terminal-width map-syntax",
        ),
        "diff" => flags_ok(
            args,
            "abBcdeEfHiNnpqrsTtuwyZ",
            "CUDFILWxXS",
            "brief unified context recursive new-file text ignore-case ignore-all-space \
             ignore-space-change ignore-blank-lines side-by-side report-identical-files minimal \
             speed-large-files strip-trailing-cr expand-tabs initial-tab ed normal \
             suppress-common-lines show-c-function color",
            "unified context label width exclude ignore-matching-lines tabsize",
        ),
        "file" => flags_ok(
            args,
            "bcdhiIkLnNprsvzZ",
            "fmeFP",
            "brief mime mime-type mime-encoding dereference no-dereference keep-going \
             special-files uncompress",
            "files-from magic-file separator",
        ),
        "git" => {
            let mut it = args.iter();
            let mut sub = None;
            // `-c k=v` can name a program (`diff.external`, `core.pager`).
            let mut config = false;
            while let Some(a) = it.next() {
                match a.as_str() {
                    "-C" => {
                        it.next();
                    }
                    "-c" => {
                        config = true;
                        it.next();
                    }
                    x if x.starts_with("--config-env") => config = true,
                    x if x.starts_with('-') => {}
                    x => {
                        sub = Some(x);
                        break;
                    }
                }
            }
            // `git grep -O<cmd>` / `--open-files-in-pager=<cmd>` runs the command
            // (every `--o…` spelling, abbreviations included, fails closed).
            let runs_pager = sub == Some("grep")
                && args.iter().any(|a| {
                    (a.starts_with("--o") && a != "--only-matching")
                        || (a.starts_with('-') && !a.starts_with("--") && a.contains('O'))
                });
            // `--output=<file>`, abbreviations included (`--out=f`).
            let writes_file = args.iter().any(|a| {
                let name = a.split('=').next().unwrap_or(a);
                name.len() >= 4 && "--output".starts_with(name) || a.starts_with("--output")
            });
            sub.is_some_and(|s| GIT_READ.contains(&s)) && !config && !runs_pager && !writes_file
        }
        _ => false,
    }
}

/// True when every flag in `args` is in the exact tables: `no` and `val` are
/// the short letters that take no value / a value (the rest of the cluster, or
/// the next word), `lno` and `lval` the space-separated long names. A letter or
/// long name that is not listed (an abbreviation included) is not a read.
fn flags_ok(args: &[String], no: &str, val: &str, lno: &str, lval: &str) -> bool {
    let has = |list: &str, n: &str| list.split_whitespace().any(|x| x == n);
    let mut it = args.iter();
    let mut done = false;
    while let Some(a) = it.next() {
        let a = a.as_str();
        if done || a == "-" || !a.starts_with('-') {
            continue;
        }
        if a == "--" {
            done = true;
        } else if let Some(l) = a.strip_prefix("--") {
            let (n, eq) = l.split_once('=').map_or((l, false), |(n, _)| (n, true));
            if has(lval, n) {
                if !eq {
                    it.next();
                }
            } else if !has(lno, n) {
                return false;
            }
        } else {
            let cl = &a[1..];
            for (k, c) in cl.char_indices() {
                if no.contains(c) {
                    continue;
                }
                if val.contains(c) {
                    if cl[k + c.len_utf8()..].is_empty() {
                        it.next();
                    }
                    break;
                }
                return false;
            }
        }
    }
    true
}

/// `awk` reads when its flags are only `-F`, `-v` and `--`, and its program
/// text has no way to write or run anything: no `>`, `|`, `system`, `getline`
/// or `@` (a gawk extension). A program in a file (`-f`) is not read-only.
fn awk_reads(args: &[String]) -> bool {
    let mut it = args.iter();
    let mut done = false;
    let mut program = false;
    while let Some(a) = it.next() {
        let a = a.as_str();
        if done || a == "-" || !a.starts_with('-') {
            if !program {
                program = true;
                if a.contains(['>', '|', '@']) || a.contains("system") || a.contains("getline") {
                    return false;
                }
            }
        } else if a == "--" {
            done = true;
        } else if a == "-F" || a == "-v" {
            it.next();
        } else if !(a.starts_with("-F") || a.starts_with("-v")) {
            return false;
        }
    }
    true
}

/// `uniq [OPTION]… [INPUT [OUTPUT]]`: the second operand is the file it
/// writes, so it reads only with at most one operand. A long option it cannot
/// place (an abbreviation of one that takes a value) fails closed.
fn uniq_reads(args: &[String]) -> bool {
    const VALUED: [&str; 3] = ["--skip-fields", "--skip-chars", "--check-chars"];
    let mut operands = 0;
    let mut done = false;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let a = a.as_str();
        if done || a == "-" || !a.starts_with('-') {
            operands += 1;
        } else if a == "--" {
            done = true;
        } else if a.starts_with("--") {
            if VALUED.contains(&a) {
                it.next();
            } else if !a.contains('=') && VALUED.iter().any(|v| v.starts_with(a)) {
                return false;
            }
        } else if a.ends_with(['f', 's', 'w']) {
            // `-f N`, `-s N`, `-w N` (the value is the next word)
            it.next();
        }
    }
    operands <= 1
}

/// `sed` reads unless a flag or a command of its script writes or runs a
/// program: `-i`/`-I`/`--in-place` (abbreviations too), `-f` (a script file we
/// cannot see), a `w`/`W`/`e` command, an `s///w` or `s///e` flag. The script
/// is parsed against an allow-list of commands; anything it does not
/// recognise is a writer, and it must pass whether or not `[...]` hides a
/// delimiter (GNU and BSD disagree).
fn sed_reads(args: &[String]) -> bool {
    let opt = |name: &str, full: &str| !name.is_empty() && full.starts_with(name);
    let mut scripts: Vec<&str> = Vec::new();
    let mut first_operand: Option<&str> = None;
    let mut have_e = false;
    let mut done = false;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let a = a.as_str();
        if done || a == "-" || !a.starts_with('-') {
            first_operand.get_or_insert(a);
        } else if a == "--" {
            done = true;
        } else if let Some(long) = a.strip_prefix("--") {
            let (name, val) = match long.split_once('=') {
                Some((n, v)) => (n, Some(v)),
                None => (long, None),
            };
            if opt(name, "in-place") || opt(name, "file") {
                return false;
            }
            if opt(name, "expression") {
                have_e = true;
                match val.or_else(|| it.next().map(String::as_str)) {
                    Some(v) => scripts.push(v),
                    None => return false,
                }
            } else if opt(name, "line-length") && val.is_none() {
                it.next();
            }
        } else {
            let cl = &a[1..];
            for (k, c) in cl.char_indices() {
                match c {
                    'i' | 'I' | 'f' => return false,
                    'e' => {
                        have_e = true;
                        let rest = &cl[k + 1..];
                        match if rest.is_empty() {
                            it.next().map(String::as_str)
                        } else {
                            Some(rest)
                        } {
                            Some(v) => scripts.push(v),
                            None => return false,
                        }
                        break;
                    }
                    'l' => {
                        if cl[k + 1..].is_empty() {
                            it.next();
                        }
                        break;
                    }
                    _ => {}
                }
            }
        }
    }
    if !have_e {
        match first_operand {
            Some(s) => scripts.push(s),
            None => return false,
        }
    }
    scripts
        .iter()
        .all(|s| sed_script_reads(s, false) && sed_script_reads(s, true))
}

/// True when every command of the sed script `s` is on the read-only list.
fn sed_script_reads(s: &str, brackets: bool) -> bool {
    let c: Vec<char> = s.chars().collect();
    let n = c.len();
    // `i` is the opening delimiter; the index just past the closing one.
    let skip = |i: usize| -> Option<usize> {
        let d = *c.get(i)?;
        if d == '\\' || d == '\n' {
            return None;
        }
        let mut j = i + 1;
        while j < n {
            match c[j] {
                '\\' => j += 2,
                x if x == d => return Some(j + 1),
                '[' if brackets => {
                    j += 1;
                    if c.get(j) == Some(&'^') {
                        j += 1;
                    }
                    if c.get(j) == Some(&']') {
                        j += 1;
                    }
                    while j < n && c[j] != ']' {
                        j += 1;
                    }
                    j += 1;
                }
                _ => j += 1,
            }
        }
        None
    };
    let eol = |mut j: usize| {
        while j < n && c[j] != '\n' {
            j += 1;
        }
        j
    };
    let mut i = 0;
    while i < n {
        match c[i] {
            ' ' | '\t' | '\n' | ';' | '{' | '}' | '!' | ',' | '~' | '+' | '$' => i += 1,
            x if x.is_ascii_digit() => i += 1,
            '#' => i = eol(i),
            '/' | '\\' => {
                let start = if c[i] == '\\' { i + 1 } else { i };
                let Some(j) = skip(start) else { return false };
                i = j;
                while i < n && matches!(c[i], 'I' | 'M') {
                    i += 1;
                }
            }
            'p' | 'P' | 'd' | 'D' | 'q' | 'Q' | 'n' | 'N' | 'g' | 'G' | 'h' | 'H' | 'x' | 'l'
            | '=' | 'z' | 'F' => i += 1,
            's' | 'y' => {
                let subst = c[i] == 's';
                let Some(j) = skip(i + 1).and_then(|j| skip(j - 1)) else {
                    return false;
                };
                i = j;
                while i < n && !matches!(c[i], ';' | '\n' | '}' | ' ' | '\t') {
                    // `w` and `e` write or run; anything unknown fails closed.
                    if !subst
                        || !(matches!(c[i], 'g' | 'p' | 'i' | 'I' | 'm' | 'M')
                            || c[i].is_ascii_digit())
                    {
                        return false;
                    }
                    i += 1;
                }
            }
            // text and file names run to the end of the line (`a\` continues)
            'a' | 'i' | 'c' => {
                i = eol(i);
                while i > 0 && i < n && c[i - 1] == '\\' {
                    i = eol(i + 1);
                }
            }
            // A file name ends at the newline, whatever precedes it: a
            // backslash there must not hide the next line's `w`.
            'r' | 'R' => i = eol(i),
            'b' | 't' | 'T' | ':' => {
                while i < n && !matches!(c[i], ';' | '\n') {
                    i += 1;
                }
            }
            _ => return false,
        }
    }
    true
}

/// Any other command naming the daemon's `qq.db`, bar the read-only tools.
fn names_db(base: &str, a: &[String]) -> Found {
    let names = a.iter().any(|x| x.rsplit('/').next() == Some("qq.db"))
        || (is_interp(base) && a.iter().any(|x| x.contains("qq.db")));
    if names && !read_only_cmd(base, a) {
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
            f.absorb(merge_hit(check_words(&cmd)));
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

/// `gh pr merge`, a merge endpoint through `gh api`, a merge mutation: a plain,
/// readable `pr merge` that starts the simple command is on the approval list
/// (the owner's prompt at every tier); the bypass flags, a word the tokenizer
/// drops or bash expands/globs/braces, and a merge behind a wrapper or
/// `find -exec` are hard blocks; a ref/contents write or a hidden query is a
/// push by another name: blocked when it lands on main/master, else on the
/// approval list.
fn check_gh(a: &[String]) -> Found {
    // `-R/--repo` names another repo than the cwd's.
    let other = a.iter().any(|x| {
        x == "-R" || x.starts_with("--repo") || (x.starts_with("-R") && !x.starts_with("--"))
    });
    let t = if other { Target::Unknown } else { Target::Here };
    check_gh_cmd(a).retarget(&t)
}

/// `gh pr merge`: an admin bypass, an unattended auto-merge, or arming or
/// disarming one. No shorthand carries them in gh 2.46 (`-A` is
/// `--author-email`).
fn merge_forbidden(name: &str) -> bool {
    matches!(name, "admin" | "auto" | "disable-auto")
}

/// A merge flag that carries its value in the next word (`-R`, `--repo`, …).
fn merge_takes_value(name: &str) -> bool {
    matches!(
        name,
        "R" | "A"
            | "b"
            | "F"
            | "t"
            | "repo"
            | "author-email"
            | "body"
            | "body-file"
            | "match-head-commit"
            | "subject"
    )
}

/// A boolean merge flag the reader knows. A name not listed here, and not a
/// value flag, is unreadable and stays a hard block.
fn merge_boolean(name: &str) -> bool {
    matches!(
        name,
        "d" | "m" | "r" | "s" | "h" | "delete-branch" | "merge" | "rebase" | "squash" | "help"
    )
}

/// The words before `pr` and between `pr` and `merge`: gh's global and
/// inherited flags only, each with its value. A subcommand word, an unknown
/// flag or a `$` expansion there means the scanner cannot read the merge.
fn gh_globals_readable(a: &[String], from: usize, to: usize) -> bool {
    let mut i = from;
    while i < to {
        let x = &a[i];
        i += 1;
        if merge_word_unreadable(x) {
            return false;
        }
        if matches!(x.as_str(), "-R" | "--repo" | "--hostname") {
            if a.get(i).is_some_and(|v| merge_word_unreadable(v)) {
                return false;
            }
            i += 1;
            continue;
        }
        let ok = matches!(x.as_str(), "-h" | "--help" | "--version" | "--")
            || x.starts_with("--repo=")
            || x.starts_with("--hostname=")
            || (x.starts_with("-R") && x.len() > 2 && !x.starts_with("--"));
        if !ok {
            return false;
        }
    }
    true
}

/// The words after `merge`: the PR argument and the merge flags the reader
/// knows. Anything else, a forbidden flag included, is not read. A separate
/// value stays in the stream: a word the tokenizer dropped (`''`, `$(…)`) can
/// shift the slot, so a flag-looking value is judged as the flag it spells.
fn gh_merge_args_readable(a: &[String], m: usize) -> bool {
    let mut i = m + 1;
    while i < a.len() {
        let x = &a[i];
        i += 1;
        if merge_word_unreadable(x) {
            return false;
        }
        if x == "--" {
            continue;
        }
        if let Some(rest) = x.strip_prefix("--") {
            let name = rest.split('=').next().unwrap_or(rest);
            if merge_forbidden(name) || !(merge_boolean(name) || merge_takes_value(name)) {
                return false;
            }
            continue;
        }
        if x.len() > 1 && x.starts_with('-') {
            let cluster = &x[1..];
            let mut at = 0;
            while at < cluster.len() {
                let c = cluster[at..].chars().next().unwrap_or_default();
                let name = c.to_string();
                if !(merge_boolean(&name) || merge_takes_value(&name)) {
                    return false;
                }
                at += c.len_utf8();
                if merge_takes_value(&name) {
                    // The rest of the cluster is its value; a separate word is
                    // judged on its own (see above).
                    break;
                }
            }
            continue;
        }
        // The PR argument: a number, URL or branch, as written.
        if x.contains('$') {
            return false;
        }
    }
    true
}

/// The positions of `pr` and `merge` when the words after `gh` name a merge.
fn gh_merge_at(a: &[String]) -> Option<(usize, usize)> {
    let p = a.iter().position(|x| x == "pr")?;
    let m = p + 1 + a[p + 1..].iter().position(|x| x == "merge")?;
    Some((p, m))
}

/// A merge word that carries a mark from [`mark_drops`].
const DROP: &str = "__qq_drop__";

/// The index where a simple command's words become `gh … pr … merge`. A
/// non-zero index means a word in front of gh (a wrapper, an assignment, a
/// keyword): the fence does not read a merge reached through one.
fn gh_merge_in(s: &[String]) -> Option<usize> {
    (0..s.len()).find(|&j| {
        let w = &s[j..];
        w[0].rsplit('/').next().unwrap_or(&w[0]) == "gh" && gh_merge_at(&w[1..]).is_some()
    })
}

/// A word bash would expand, glob or brace-expand into other words: a merge
/// argument holding one is not the text gh receives. Quotes and spaces are
/// already resolved in a token, so they are plain here.
fn merge_word_unreadable(w: &str) -> bool {
    w.contains(DROP)
        || w.chars().any(|c| {
            matches!(
                c,
                '$' | '`'
                    | '{'
                    | '}'
                    | '*'
                    | '?'
                    | '['
                    | ']'
                    | '~'
                    | '<'
                    | '>'
                    | '('
                    | ')'
                    | ';'
                    | '&'
                    | '|'
                    | '!'
                    | '\\'
                    | '\n'
            )
        })
}

/// The end of the substitution that starts at `i`: a backtick, or `$(`/`<(`/`>(`
/// with `i` on the `$`/`<`/`>` and `i + 1` the `(`. Quote-aware and
/// parenthesis-nested, like the tokenizer's own `subst`.
fn skip_subst(b: &[char], i: usize) -> usize {
    if b[i] == '`' {
        let mut j = i + 1;
        while j < b.len() {
            if b[j] == '\\' {
                j += 2;
                continue;
            }
            if b[j] == '`' {
                return j + 1;
            }
            j += 1;
        }
        return b.len();
    }
    let mut j = i + 2;
    let mut depth = 1usize;
    let mut quote = '\0';
    while j < b.len() {
        let c = b[j];
        match quote {
            '\'' => {
                if c == '\'' {
                    quote = '\0';
                }
                j += 1;
                continue;
            }
            '"' => {
                if c == '\\' {
                    j += 2;
                    continue;
                }
                if c == '"' {
                    quote = '\0';
                }
                j += 1;
                continue;
            }
            _ => {}
        }
        match c {
            '\'' | '"' => quote = c,
            '\\' => j += 1,
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    return j + 1;
                }
            }
            _ => {}
        }
        j += 1;
    }
    b.len()
}

/// Copy `src` with every word the tokenizer would drop replaced by [`DROP`]:
/// command and process substitutions, backticks, and empty quoted words. A
/// second tokenize pass then says which simple command carried the drop.
fn mark_drops(src: &str) -> String {
    let b: Vec<char> = src.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        match c {
            '\'' => {
                if i + 1 < b.len() && b[i + 1] == '\'' {
                    out.push_str(DROP);
                    i += 2;
                    continue;
                }
                // No substitution inside single quotes: copy it verbatim.
                out.push(c);
                i += 1;
                while i < b.len() {
                    out.push(b[i]);
                    if b[i] == '\'' {
                        i += 1;
                        break;
                    }
                    i += 1;
                }
            }
            '"' => {
                if i + 1 < b.len() && b[i + 1] == '"' {
                    out.push_str(DROP);
                    i += 2;
                    continue;
                }
                out.push(c);
                i += 1;
                while i < b.len() {
                    let d = b[i];
                    if d == '\\' && i + 1 < b.len() {
                        out.push(d);
                        out.push(b[i + 1]);
                        i += 2;
                        continue;
                    }
                    if d == '"' {
                        out.push(d);
                        i += 1;
                        break;
                    }
                    if d == '`' || (d == '$' && i + 1 < b.len() && b[i + 1] == '(') {
                        out.push_str(DROP);
                        i = skip_subst(&b, i);
                        continue;
                    }
                    out.push(d);
                    i += 1;
                }
            }
            '\\' => {
                out.push(c);
                if i + 1 < b.len() {
                    out.push(b[i + 1]);
                    i += 2;
                } else {
                    i += 1;
                }
            }
            '`' => {
                out.push_str(DROP);
                i = skip_subst(&b, i);
            }
            '$' if i + 1 < b.len() && b[i + 1] == '(' => {
                out.push_str(DROP);
                i = skip_subst(&b, i);
            }
            '<' | '>' if i + 1 < b.len() && b[i + 1] == '(' => {
                out.push_str(DROP);
                i = skip_subst(&b, i + 1);
            }
            _ => {
                out.push(c);
                i += 1;
            }
        }
    }
    out
}

fn check_gh_cmd(a: &[String]) -> Found {
    if let Some((p, m)) = gh_merge_at(a) {
        let readable = gh_globals_readable(a, 0, p)
            && gh_globals_readable(a, p + 1, m)
            && gh_merge_args_readable(a, m);
        return if readable {
            Found::act(Action::MergeRequest, ASK_MERGE)
        } else {
            Found::block(MERGE_MESSAGE)
        };
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
fn check_git(a: &[String], deep: bool) -> Found {
    let mut target = Target::Here;
    let f = check_git_at(a, &mut target, deep);
    f.retarget(&target)
}

/// `-C a -C b` chain; a path the scanner cannot read is unknown.
fn chain_dir(prev: &Target, d: Option<&String>) -> Target {
    let Some(d) = d.filter(|d| !d.is_empty() && !d.contains(['$', '`', '*', '?'])) else {
        return Target::Unknown;
    };
    match prev {
        Target::Unknown => Target::Unknown,
        Target::Dir(p) if !d.starts_with('/') && !d.starts_with('~') => {
            Target::Dir(format!("{p}/{d}"))
        }
        _ => Target::Dir(d.clone()),
    }
}

/// git's global options: an exact table, like the wrapper tables. `NOVAL` take
/// no value; `VAL` take one (`--opt v` or `--opt=v`); `--exec-path` and
/// `--list-cmds` only take it glued.
const GIT_NOVAL: &[&str] = &[
    "-v",
    "--version",
    "-p",
    "--paginate",
    "-P",
    "--no-pager",
    "--no-replace-objects",
    "--no-lazy-fetch",
    "--no-optional-locks",
    "--no-advice",
    "--bare",
    "--literal-pathspecs",
    "--glob-pathspecs",
    "--noglob-pathspecs",
    "--icase-pathspecs",
    "--html-path",
    "--man-path",
    "--info-path",
];
const GIT_VAL: &[&str] = &[
    "-C",
    "-c",
    "--config-env",
    "--git-dir",
    "--work-tree",
    "--namespace",
    "--super-prefix",
    "--attr-source",
];
const GIT_GLUED: &[&str] = &["--list-cmds"];

/// `-c` / `--config-env` keys that are plain values, never a program or an
/// alias. Any other key makes the call unknown.
fn safe_git_key(k: &str) -> bool {
    let k = k.to_ascii_lowercase();
    k.starts_with("advice.")
        || matches!(
            k.as_str(),
            "user.name"
                | "user.email"
                | "commit.gpgsign"
                | "tag.gpgsign"
                | "core.quotepath"
                | "core.abbrev"
                | "core.autocrlf"
                | "init.defaultbranch"
                | "color.ui"
                | "safe.directory"
                | "protocol.version"
                | "gc.auto"
                | "pull.rebase"
                | "pull.ff"
                | "fetch.prune"
                | "push.autosetupremote"
                | "rebase.autostash"
                | "merge.ff"
        )
}

fn names_push_token(v: &str) -> bool {
    v.split(|c: char| !c.is_ascii_alphanumeric() && c != '_' && c != '-')
        .any(|t| t == "push")
}

/// The call is one the fast path cannot read: ask (workers block). A push word
/// anywhere after `from` is a hard deny.
fn unknown_git(a: &[String], from: usize) -> Found {
    if a.get(from..)
        .unwrap_or_default()
        .iter()
        .any(|x| names_push_token(x))
    {
        Found::block(PUSH_MESSAGE)
    } else {
        Found::act(Action::Bypass, ASK_BYPASS)
    }
}

/// One `-c k=v` / `--config-env k=ENV` word names a key outside the safe list.
/// A plain alias (`alias.st=status`) stays free; a shell alias (`!…`) or one
/// naming a push does not.
/// `from_env`: the value is an environment variable's name, unreadable here.
fn git_config_unsafe(cfg: &str, from_env: bool) -> bool {
    let (key, val) = cfg.split_once('=').unwrap_or((cfg, ""));
    if !from_env && strip_alias(key).is_some() {
        return val.starts_with('!') || names_push_token(val);
    }
    !safe_git_key(key)
}

/// Assignment words that carry git config or name a program git will run
/// (`GIT_CONFIG_*`, `GIT_EXTERNAL_DIFF`, `GIT_EDITOR`, `EDITOR`, …): an alias or
/// program the gate cannot read. Plain values (`GIT_CONFIG_GLOBAL=/dev/null`,
/// `GIT_EDITOR=true`) stay free.
fn git_env_words(seg: &[String]) -> Found {
    let mut f = Found::pass();
    for w in seg {
        // `NAME=v` and `NAME+=v`.
        let Some((n, v)) = w.split_once('=') else {
            continue;
        };
        let n = n.strip_suffix('+').unwrap_or(n);
        if !is_assignment(&format!("{n}=")) {
            continue;
        }
        let name = n.to_ascii_uppercase();
        let unsafe_cfg = if name.starts_with("GIT_CONFIG_KEY_") {
            !safe_git_key(v)
        } else if name == "GIT_CONFIG_PARAMETERS" {
            v.replace('\'', "")
                .split_whitespace()
                .any(|kv| !safe_git_key(kv.split_once('=').map_or(kv, |x| x.0)))
        } else if matches!(
            name.as_str(),
            "GIT_CONFIG" | "GIT_CONFIG_GLOBAL" | "GIT_CONFIG_SYSTEM"
        ) {
            !(v.is_empty() || v == "/dev/null")
        } else if name == "GIT_EXEC_PATH" {
            !v.is_empty()
        } else if matches!(
            name.as_str(),
            "GIT_EXTERNAL_DIFF"
                | "GIT_EDITOR"
                | "GIT_SEQUENCE_EDITOR"
                | "GIT_SSH"
                | "GIT_SSH_COMMAND"
                | "GIT_PAGER"
                | "GIT_ASKPASS"
                | "GIT_PROXY_COMMAND"
                | "EDITOR"
                | "VISUAL"
                | "PAGER"
                | "SSH_ASKPASS"
        ) {
            // A program: only the plain ones are free. Its command line is also
            // read like any other, so a push in it is a hard deny.
            if v.contains(char::is_whitespace) || v.contains(';') {
                f.absorb(scan_with(v, false));
            }
            !matches!(
                v,
                "" | "true"
                    | "false"
                    | ":"
                    | "cat"
                    | "less"
                    | "more"
                    | "vi"
                    | "vim"
                    | "nvim"
                    | "nano"
                    | "emacs"
                    | "ssh"
                    | "code --wait"
            )
        } else {
            false
        };
        if unsafe_cfg {
            f.absorb(if names_push_token(v) && !name.starts_with("GIT_CONFIG") {
                Found::block(PUSH_MESSAGE)
            } else {
                Found::act(Action::Bypass, ASK_BYPASS)
            });
        }
    }
    f
}

fn check_git_at(a: &[String], target: &mut Target, deep: bool) -> Found {
    let mut i = 0;
    while i < a.len() {
        let w = a[i].as_str();
        let (name, glued) = match w.split_once('=') {
            Some((n, _)) if n.starts_with("--") => (n, true),
            _ => (w, false),
        };
        if w == "-c" || (w.starts_with("-c") && w.len() > 2 && !w.starts_with("--")) {
            let cfg = if w == "-c" { a.get(i + 1) } else { Some(&a[i]) };
            let cfg = cfg.map(|c| c.strip_prefix("-c").unwrap_or(c)).unwrap_or("");
            let next = i + if w == "-c" { 2 } else { 1 };
            if git_config_unsafe(cfg, false) {
                // A plain `-c k=v` value is scanned too; an alias keeps its ask.
                let alias = strip_alias(cfg.split_once('=').map_or(cfg, |x| x.0)).is_some();
                return unknown_git(a, if alias { next } else { i + 1 });
            }
            i = next;
        } else if name == "--config-env" {
            let cfg = if glued {
                w.split_once('=').map(|x| x.1)
            } else {
                a.get(i + 1).map(String::as_str)
            };
            let next = i + if glued { 1 } else { 2 };
            if git_config_unsafe(cfg.unwrap_or(""), true) {
                return unknown_git(a, next);
            }
            i = next;
        } else if w == "-C" {
            *target = chain_dir(target, a.get(i + 1));
            i += 2;
        } else if w == "--help" || w == "-h" {
            return Found::pass();
        } else if GIT_VAL.contains(&name) {
            if matches!(name, "--git-dir" | "--work-tree") {
                *target = Target::Unknown;
            }
            i += if glued { 1 } else { 2 };
        } else if GIT_NOVAL.contains(&w) || (glued && GIT_GLUED.contains(&name)) {
            i += 1;
        } else if w.starts_with('-') {
            return unknown_git(a, i + 1);
        } else {
            break;
        }
    }
    let rest = a.get(i + 1..).unwrap_or_default();
    match a.get(i).map(String::as_str) {
        Some("config") => check_git_config(rest),
        Some("branch") => check_git_branch(rest),
        Some("tag") => check_git_tag(rest),
        // Verbs that run a command line of their own (`rebase -x`, `bisect run`,
        // `submodule foreach`, `--upload-pack=`): each word may start one.
        Some(
            "rebase" | "bisect" | "submodule" | "fetch" | "pull" | "ls-remote" | "clone"
            | "difftool" | "mergetool" | "remote",
        ) if deep => scan_suffixes(rest, 0),
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
                Some(names) if !names.is_empty() => {
                    let remote = rest.iter().find(|x| !x.starts_with('-')).cloned();
                    delete_hits(names, remote)
                }
                _ => Found::block(PUSH_MESSAGE),
            }
        }
        _ => Found::pass(),
    }
}

fn delete_hits(names: Vec<String>, remote: Option<String>) -> Found {
    let mut f = Found::pass();
    for n in names {
        f.listed.push(Hit {
            p: Pending::BranchDelete(n, remote.clone()),
            reason: ASK_BRANCH_DELETE,
            target: Target::Here,
        });
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

/// `git config` that sets a key outside the safe list: a program, a file of
/// more config, a shell alias or an alias naming a push. A literal push in the
/// value of a non-alias key is a hard deny. Reads stay free.
fn check_git_config(a: &[String]) -> Found {
    // One pass over the arguments: a flag that takes a value consumes it (so the
    // value is neither a flag nor a word); the rest are flags and positionals.
    let mut p: Vec<&str> = Vec::new();
    let mut reads = false;
    let mut it = a.iter();
    while let Some(x) = it.next() {
        if matches!(
            x.as_str(),
            "-f" | "--file" | "--blob" | "-t" | "--type" | "--default" | "--comment"
        ) {
            it.next();
        } else if x.starts_with('-') {
            reads |= x.starts_with("--get")
                || x.starts_with("--unset")
                || matches!(
                    x.as_str(),
                    "-l" | "--list" | "-e" | "--edit" | "--remove-section"
                );
        } else {
            p.push(x);
        }
    }
    if p.first().is_some_and(|x| *x == "set") {
        p.remove(0);
    } else if p
        .first()
        .is_some_and(|x| matches!(*x, "get" | "list" | "unset" | "edit"))
    {
        return Found::pass();
    }
    if reads || p.len() < 2 || !git_config_unsafe(&format!("{}={}", p[0], p[1]), false) {
        return Found::pass();
    }
    let alias = strip_alias(p[0]).is_some();
    if !alias && p[1..].iter().any(|v| names_push_token(v)) {
        Found::block(PUSH_MESSAGE)
    } else {
        Found::act(Action::Bypass, ASK_BYPASS)
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
        return delete_hits(names.iter().map(|n| n.to_string()).collect(), None);
    }
    let moves = long("--move") || short('m') || short('M');
    let copies = long("--copy") || short('c') || short('C');
    let forces = long("--force") || short('f');
    if ((moves || copies) && names.iter().any(|n| is_trunk(n)))
        || (forces && names.first().is_some_and(|n| is_trunk(n)))
    {
        return Found::act(Action::TrunkMove, ASK_TRUNK_MOVE);
    }
    Found::pass()
}

/// `git tag` that creates, moves or deletes a tag; listing and reading are free.
/// A list flag (`-l`, `-n`, `-v`, `--contains`, `--points-at`, `--merged`)
/// makes a bare word a pattern; `--sort`, `--format` and friends do not, so
/// `git tag --sort=refname v9` still creates `v9`.
fn check_git_tag(a: &[String]) -> Found {
    let (mut list, mut write, mut positional) = (false, false, false);
    let mut k = 0;
    while k < a.len() {
        let x = a[k].as_str();
        k += 1;
        if let Some(long) = x.strip_prefix("--") {
            let (name, joined) = match long.split_once('=') {
                Some((n, _)) => (n, true),
                None => (long, false),
            };
            match name {
                "list" | "verify" | "contains" | "no-contains" | "points-at" | "merged"
                | "no-merged" => list = true,
                "delete" | "annotate" | "sign" | "force" | "message" | "file" | "local-user" => {
                    write = true
                }
                "sort" | "format" if !joined => k += 1,
                _ => {}
            }
        } else if x.len() > 1 && x.starts_with('-') {
            for c in x[1..].chars() {
                match c {
                    'l' | 'v' => list = true,
                    'n' => {
                        list = true;
                        break;
                    }
                    'a' | 's' | 'd' | 'f' | 'm' | 'u' | 'F' => write = true,
                    _ => {}
                }
            }
        } else {
            positional = true;
        }
    }
    if write || (positional && !list) {
        Found::act(Action::Tag, ASK_TAG)
    } else {
        Found::pass()
    }
}

/// The exact spellings of one wrapper's leading options.
struct WrapTable {
    /// Short letters that take no value / a value (the rest of the cluster, or
    /// the next word) / an optional value (only the rest of the cluster).
    no: &'static str,
    val: &'static str,
    opt: &'static str,
    /// Long names (space separated) that take no value / a value.
    lno: &'static str,
    lval: &'static str,
    /// Short letters and long names that move the command to another directory.
    chdir: &'static str,
    lchdir: &'static str,
}

fn wrap_table(base: &str) -> WrapTable {
    let t = |no, val, opt, lno, lval, chdir, lchdir| WrapTable {
        no,
        val,
        opt,
        lno,
        lval,
        chdir,
        lchdir,
    };
    match base {
        "sudo" => t(
            "ABbEeHiKklnPSsVvN",
            "ughpCTURtDrc",
            "",
            "askpass background bell preserve-env login non-interactive reset-timestamp stdin \
             shell validate version remove-timestamp list set-home edit",
            "user group host prompt close-from command-timeout other-user chroot chdir role type",
            "DR",
            "chdir chroot",
        ),
        "env" => t(
            "i0v",
            "uCSPa",
            "",
            "ignore-environment null debug list-signal-handling",
            "unset chdir split-string argv0 block-signal default-signal ignore-signal",
            "C",
            "chdir",
        ),
        "nice" => t("0123456789", "n", "", "", "adjustment", "", ""),
        "timeout" => t(
            "fpv",
            "sk",
            "",
            "foreground preserve-status verbose",
            "signal kill-after",
            "",
            "",
        ),
        "xargs" => t(
            "0prtxoh",
            "InLPsdEaJRS",
            "iel",
            "null no-run-if-empty verbose interactive exit open-tty show-limits replace eof",
            "max-args max-lines max-procs max-chars delimiter arg-file",
            "",
            "",
        ),
        "chrt" => t(
            "bfiomrRv",
            "",
            "",
            "batch fifo idle other rr reset-on-fork",
            "",
            "",
            "",
        ),
        "ionice" => t(
            "t",
            "cnpPu",
            "",
            "ignore",
            "class classdata pid pgid uid",
            "",
            "",
        ),
        "taskset" => t("cp", "", "", "cpu-list pid", "", "", ""),
        "stdbuf" => t("", "ioe", "", "", "input output error", "", ""),
        "time" => t(
            "apvV",
            "fo",
            "",
            "append portability verbose",
            "format output",
            "",
            "",
        ),
        "command" => t("pvV", "", "", "", "", "", ""),
        "exec" => t("cl", "a", "", "", "", "", ""),
        "setsid" => t("cfwV", "", "", "ctty fork wait", "", "", ""),
        _ => t("", "", "", "", "", "", ""),
    }
}

/// The command a wrapper runs. A wrapper's leading options are skipped only when
/// every token is in its exact table (a short cluster is read letter by letter).
/// The first token that is not makes the command word unknown, and then every
/// later word is checked as a command ([`scan_suffixes`]): no table of
/// value-taking options has to stay complete. `env -C`, `--chdir`, `sudo -D`
/// move the command to another directory.
fn wrapper(base: &str, a: &[String]) -> Found {
    let t = wrap_table(base);
    let listed = |l: &str, n: &str| l.split_whitespace().any(|x| x == n);
    let mut duration_pending = matches!(base, "timeout" | "chrt" | "taskset");
    let (mut k, mut moved, mut ended) = (0, false, false);
    // An option the table does not know: any later word may be the command.
    let unknown = |k: usize| scan_suffixes(a, k + 1);
    while k < a.len() {
        let tok = a[k].as_str();
        if !ended && tok == "--" {
            ended = true;
            k += 1;
        } else if !ended && tok.starts_with("--") {
            let (name, eq) = tok[2..]
                .split_once('=')
                .map_or((&tok[2..], None), |(n, v)| (n, Some(v)));
            let takes = listed(t.lval, name);
            if !takes && !listed(t.lno, name) {
                return unknown(k);
            }
            moved |= listed(t.lchdir, name);
            if name == "split-string" {
                if let Some(v) = eq.or_else(|| a.get(k + 1).map(String::as_str)) {
                    let f = scan(v);
                    if !f.is_pass() {
                        return f;
                    }
                }
            }
            k += if takes && eq.is_none() { 2 } else { 1 };
        } else if !ended && tok == "-" && base == "env" {
            k += 1;
        } else if !ended && tok.starts_with('-') && tok.len() > 1 {
            let mut next_taken = false;
            for (p, c) in tok.char_indices().skip(1) {
                if t.no.contains(c) {
                    continue;
                }
                if !t.val.contains(c) && !t.opt.contains(c) {
                    return unknown(k);
                }
                moved |= t.chdir.contains(c);
                let rest = &tok[p + c.len_utf8()..];
                let value = if !rest.is_empty() {
                    Some(rest)
                } else if t.val.contains(c) {
                    next_taken = true;
                    a.get(k + 1).map(String::as_str)
                } else {
                    None
                };
                // `env -S 'cmd args'`: the value is a command line.
                if base == "env" && c == 'S' {
                    if let Some(v) = value {
                        let f = scan(v);
                        if !f.is_pass() {
                            return f;
                        }
                    }
                }
                break;
            }
            k += 1 + usize::from(next_taken);
        } else if is_assignment(tok) {
            moved |= retargets(tok);
            k += 1;
        } else if duration_pending {
            duration_pending = false;
            k += 1;
        } else {
            let target = if moved { Target::Unknown } else { Target::Here };
            return check_words(&a[k..]).retarget(&target);
        }
    }
    Found::pass()
}

/// The command word is unknown (an unlisted command, or a wrapper option the
/// table does not list): any word from `from` on may start the command that
/// runs. Each suffix is read as a command if it opens with a known command word,
/// and each word that holds a space as a command line whose own command word
/// must be known (prose is not a command). The hits are judged at an unknown
/// target. Wrappers inside are not followed: their own words are suffixes here.
fn scan_suffixes(a: &[String], from: usize) -> Found {
    let mut f = Found::pass();
    for j in from..a.len() {
        f.absorb(check_flat(&a[j..]));
        f.absorb(spaced_word(&a[j]));
    }
    f.moved |= !f.is_pass();
    f.force_target(&Target::Unknown)
}

/// A word that holds a command line: the word itself, and for an option the text
/// after `=` and after each cluster letter (`--command='git push'`,
/// `-qc'git push'`). The command word of the line must be known.
fn spaced_word(x: &str) -> Found {
    let mut f = Found::pass();
    if !x.contains(char::is_whitespace) {
        return f;
    }
    f.absorb(scan_with(x, false));
    if x.starts_with('-') {
        if let Some((_, v)) = x.split_once('=') {
            f.absorb(scan_with(v, false));
        }
        if !x.starts_with("--") {
            for (p, c) in x.char_indices().skip(1) {
                f.absorb(scan_with(&x[p + c.len_utf8()..], false));
            }
        }
    }
    f
}

/// One suffix as a command through the ordinary path, shallow: a wrapper word
/// passes (its own words are suffixes of the same scan), and no suffix scan runs
/// inside (that would be exponential in the word count). Every other check of
/// [`check_cmd`] (db writes, path writers, grants) still applies.
fn check_flat(w: &[String]) -> Found {
    let base = w[0].rsplit('/').next().unwrap_or(&w[0]);
    if matches!(base, "xargs" | "parallel") {
        // Shallow: no wrapper deep scan here, only the merge shape.
        return if gh_merge_in(w).is_some() {
            Found::block(MERGE_MESSAGE)
        } else {
            Found::pass()
        };
    }
    if matches!(
        base,
        "sudo"
            | "env"
            | "command"
            | "exec"
            | "nice"
            | "time"
            | "nohup"
            | "setsid"
            | "stdbuf"
            | "builtin"
            | "chrt"
            | "ionice"
            | "taskset"
            | "timeout"
    ) {
        return Found::pass();
    }
    check_cmd(w, 0, false)
}

/// A merge reached through a door that adds or hides words (`xargs`, `find
/// -exec`, `parallel`) is a hard block, never a prompt.
fn merge_hit(f: Found) -> Found {
    if f.listed
        .iter()
        .any(|h| matches!(h.p, Pending::Act(Action::MergeRequest)))
    {
        Found::block(MERGE_MESSAGE)
    } else {
        f
    }
}

/// A wrapper that adds words the text does not show (`xargs`, `parallel`): a
/// `gh pr merge` it runs is not readable, whatever its visible argv says.
fn merge_behind(base: &str, a: &[String]) -> Found {
    merge_hit(wrapper(base, a))
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
        std::fs::create_dir_all(ws.join(".claude/rules")).unwrap();
        std::fs::write(ws.join("AGENTS.md"), "v1").unwrap();
        crate::memory::record_session(&ws, "s1", "startup");
        let hook = |c: &str| {
            serde_json::json!({"session_id": "s1", "tool_input": {"command": c}}).to_string()
        };
        std::fs::write(ws.join(".claude/rules/new.md"), "rule").unwrap();
        assert!(
            drift_warning(&ws, &hook("quaqua status")).is_none(),
            "not a dispatch"
        );
        let msg = drift_warning(&ws, &hook("cd x && quaqua worker spawn a b c")).unwrap();
        assert!(msg.contains(".claude/rules/new.md"), "{msg}");
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
