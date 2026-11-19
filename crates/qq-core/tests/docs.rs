//! T47 / R25: the workspace documents shipped with the crate describe the new
//! binary. Every `quaqua` command they name exists (in the CLI registry, or in
//! [`PENDING`] while its owning task is unbuilt), and they name no removed
//! feature.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use qq_core::cli::{self, CliError, Command, Env, Install, Registry};

/// Commands the documents name that no built task provides yet, with the task
/// that owns each. Delete an entry when its task lands; `pending_commands_are_still_missing`
/// fails while an entry already exists in the registry.
const PENDING: &[(&str, &str)] = &[];

/// Modules whose `INSTALL` exists on the branch but is not in `cli::MODULES` yet.
const UNLISTED_INSTALLS: &[Install] = &[];

/// `--flag`s the documents put after a registered command. A command that is
/// itself in [`PENDING`] owns its flags; a registered command's flags are either
/// here (it accepts them) or in [`PENDING_FLAGS`] (its owning task adds them).
const ACCEPTED_FLAGS: &[(&str, &str)] = &[
    ("gate", "--json"),
    ("status", "--all"),
    ("status", "--metrics"),
    ("inbox", "--for"),
    ("inbox", "--format"),
    ("inbox", "--session"),
    ("transcripts", "--last"),
    ("transcripts", "--session"),
    ("worker", "--check-brief"),
    ("worker", "--extend"),
    ("worker", "--resume"),
    ("worker", "--base"),
    ("worker", "--purpose"),
    ("worker", "--review"),
    ("lead", "--task"),
    ("grid", "--only"),
    ("grid", "--tab"),
    ("pipeline", "--resume"),
    ("pipeline", "--rounds"),
];

/// Flags documented for a registered command that its owning task adds later.
/// Delete an entry when it lands (move it to [`ACCEPTED_FLAGS`]).
const PENDING_FLAGS: &[(&str, &str, &str)] = &[];

/// Words no shipped document may name (R15, R25). `omp` and the removed
/// task-force name are covered by the phase-check grep over the templates.
const REMOVED_WORDS: &[&str] = &["auditor", "layout_shadow"];

/// Paths no shipped workspace may contain (R17, T47).
const REMOVED_PATHS: &[&str] = &[
    "scripts/auditor",
    "scripts/github-ci-poll",
    "scripts/hooks/omp-worker-state.ts",
];

fn workspace() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("templates/workspace")
}

fn files(dir: &Path, out: &mut Vec<PathBuf>) {
    let mut entries: Vec<_> = std::fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("read {}: {e}", dir.display()))
        .map(|e| e.unwrap().path())
        .collect();
    entries.sort();
    for p in entries {
        if p.is_dir() {
            files(&p, out);
        } else {
            out.push(p);
        }
    }
}

fn all_files() -> Vec<PathBuf> {
    let mut v = Vec::new();
    files(&workspace(), &mut v);
    assert!(
        !v.is_empty(),
        "no documents under {}",
        workspace().display()
    );
    v
}

fn rel(p: &Path) -> String {
    p.strip_prefix(workspace())
        .unwrap()
        .to_string_lossy()
        .into_owned()
}

// ---- extraction ------------------------------------------------------------

fn clean(tok: &str) -> &str {
    tok.trim_matches(|c| matches!(c, '"' | '\'' | '\\' | ',' | ';' | '(' | ')'))
}

fn is_command_word(tok: &str) -> bool {
    let t = clean(tok);
    matches!(t, "quaqua" | "$QUAQUA" | "$q" | "$bin") || t.ends_with("/quaqua")
}

fn is_word(tok: &str) -> bool {
    let t = clean(tok);
    t.chars().next().is_some_and(|c| c.is_ascii_lowercase())
        && t.chars().all(|c| c.is_ascii_lowercase() || c == '-')
}

type Found = BTreeMap<String, BTreeSet<String>>;

/// The `--flag` name of a token (`--for`, `--format="x"` -> `--format`).
fn flag_of(tok: &str) -> Option<String> {
    let t = clean(tok).split('=').next()?;
    let name = t.strip_prefix("--")?;
    (!name.is_empty() && name.chars().all(|c| c.is_ascii_lowercase() || c == '-'))
        .then(|| t.to_string())
}

/// `verb` or `verb sub` for every command word in `text`; every `--flag` that
/// follows it (up to the next command word or shell separator) goes in `flags`
/// as `<verb> <flag>` for the command's top-level verb.
fn commands_in(text: &str, out: &mut Found, flags: &mut Found, file: &str) {
    let toks: Vec<&str> = text.split_whitespace().collect();
    for (i, t) in toks.iter().enumerate() {
        if !is_command_word(t) {
            continue;
        }
        let Some(verb) = toks.get(i + 1).filter(|v| is_word(v)) else {
            continue;
        };
        let verb = clean(verb);
        let key = match toks.get(i + 2).filter(|s| is_word(s)) {
            Some(sub) => format!("{verb} {}", clean(sub)),
            None => verb.to_string(),
        };
        let top = key.split(' ').next().unwrap().to_string();
        let skip = 2 + usize::from(key.contains(' '));
        for tok in toks[i + 1..].iter().skip(skip - 1) {
            if is_command_word(tok) || matches!(*tok, "|" | "||" | "&&" | ";") {
                break;
            }
            if let Some(f) = flag_of(tok) {
                flags
                    .entry(format!("{top} {f}"))
                    .or_default()
                    .insert(file.to_string());
            }
        }
        out.entry(key).or_default().insert(file.to_string());
    }
}

/// Markdown and toml name commands in code spans and fenced blocks; prose
/// ("the quaqua binary") is not a command. Shell and json name them in code.
fn documented() -> (Found, Found) {
    let (mut out, mut flags) = (Found::new(), Found::new());
    for p in all_files() {
        let text = std::fs::read_to_string(&p).unwrap();
        let file = rel(&p);
        let ext = p.extension().and_then(|e| e.to_str()).unwrap_or("");
        let mut fenced = false;
        for line in text.lines() {
            if line.trim_start().starts_with("```") {
                fenced = !fenced;
                continue;
            }
            let code_file = matches!(ext, "sh" | "json");
            // Shell comments and echoed messages are prose.
            let t = line.trim_start();
            let comment = ext == "sh" && (t.starts_with('#') || t.starts_with("echo"));
            if fenced || (code_file && !comment) {
                commands_in(line, &mut out, &mut flags, &file);
                continue;
            }
            for (n, span) in line.split('`').enumerate() {
                if n % 2 == 1 {
                    commands_in(span, &mut out, &mut flags, &file);
                }
            }
        }
    }
    (out, flags)
}

// ---- registry --------------------------------------------------------------

fn run_env() -> Env {
    let dir = std::env::temp_dir().join(format!("qq-docs-{}", std::process::id()));
    Env { runs_dir: dir }
}

/// Top-level command names: the core, `cli::MODULES` and every unlisted INSTALL.
fn registry() -> (Registry, Env) {
    let env = run_env();
    let (mut registry, mut daemon) = cli::assemble(&env).expect("assemble");
    for install in UNLISTED_INSTALLS {
        // An install already in MODULES refuses its own names: that is the
        // expected state once the Lead lists it.
        if let Err(e) = install(&mut registry, &mut daemon) {
            assert!(e.contains("already registered"), "install failed: {e}");
        }
    }
    (registry, env)
}

fn noop(_: &Env, _: &[String]) -> Result<String, CliError> {
    Ok(String::new())
}

/// Which of `words` are top-level commands. A name that is already taken
/// refuses a second registration, so each word is probed once.
fn registered(registry: &mut Registry, words: &BTreeSet<String>) -> BTreeSet<String> {
    words
        .iter()
        .filter(|w| {
            let name: &'static str = Box::leak(w.to_string().into_boxed_str());
            registry
                .register(Command {
                    name,
                    help: "probe",
                    run: noop,
                })
                .is_err()
        })
        .cloned()
        .collect()
}

type Probe = fn(&Env, &[String]) -> Result<String, CliError>;

/// Commands whose sub-verb can be checked without a daemon: an unknown
/// sub-verb is a usage error (exit 2) before any call; a known one reaches
/// the missing daemon (exit 1).
const SUB_PROBES: &[(&str, Probe)] = &[("config", qq_core::config::config_cmd)];

/// Commands that report a usage error with the same exit code as a missing
/// daemon (or take no probeable args), so they cannot be probed through the
/// command: their sub-verbs are listed here, as each module's usage text names them.
const KNOWN_SUBS: &[(&str, &[&str])] = &[
    ("grid", &["reconcile", "acquire"]),
    ("lead", &["spawn", "cleanup"]),
    ("worker", &["spawn", "prompt", "cleanup"]),
    ("identity", &["heal"]),
    ("mail", &["send"]),
    ("memory", &["hook"]),
    ("playbook", &["list", "show"]),
    ("transcripts", &["tail"]),
];

/// Does the registry provide `key` (`verb` or `verb sub`)?
fn provided(key: &str, top: &BTreeSet<String>, env: &Env) -> Result<bool, String> {
    let (verb, sub) = match key.split_once(' ') {
        Some((v, s)) => (v, Some(s)),
        None => (key, None),
    };
    if !top.contains(verb) {
        return Ok(false);
    }
    let Some(sub) = sub else { return Ok(true) };
    match SUB_PROBES.iter().find(|(v, _)| *v == verb) {
        Some((_, probe)) => {
            let args = [sub.to_string(), "x".to_string(), "x".to_string()];
            Ok(probe(env, &args).err().map(|e| e.code) != Some(2))
        }
        None => match KNOWN_SUBS.iter().find(|(v, _)| *v == verb) {
            Some((_, subs)) => Ok(subs.contains(&sub)),
            None => Err(format!(
                "`{key}`: `{verb}` exists but its sub-verb cannot be checked; add a SUB_PROBES or KNOWN_SUBS entry"
            )),
        },
    }
}

fn words(keys: impl Iterator<Item = String>) -> BTreeSet<String> {
    keys.map(|k| k.split(' ').next().unwrap().to_string())
        .collect()
}

// ---- tests -----------------------------------------------------------------

#[test]
fn workspace_docs_name_only_existing_commands() {
    let (docs, _) = documented();
    assert!(docs.len() >= 5, "extraction found too little: {docs:?}");
    let (mut reg, env) = registry();
    let top = registered(&mut reg, &words(docs.keys().cloned()));
    let pending: BTreeSet<&str> = PENDING.iter().map(|(c, _)| *c).collect();
    let mut bad = Vec::new();
    for (key, in_files) in &docs {
        match provided(key, &top, &env) {
            Ok(true) => {}
            Ok(false) if pending.contains(key.as_str()) => {}
            Ok(false) => bad.push(format!("`quaqua {key}` in {in_files:?}")),
            Err(e) => bad.push(e),
        }
    }
    assert!(
        bad.is_empty(),
        "documents name commands that do not exist (add the verb, fix the text, or list it in PENDING with its task):\n{}",
        bad.join("\n")
    );
}

#[test]
fn workspace_docs_name_only_accepted_flags() {
    let (docs, flags) = documented();
    assert!(
        flags.len() >= 5,
        "flag extraction found too little: {flags:?}"
    );
    let (mut reg, _) = registry();
    let top = registered(&mut reg, &words(docs.keys().cloned()));
    let pending_cmd: BTreeSet<&str> = PENDING.iter().map(|(c, _)| *c).collect();
    let mut bad = Vec::new();
    for (pair, in_files) in &flags {
        let (verb, flag) = pair.split_once(' ').unwrap();
        // A command that does not exist yet owns its flags; `docs` keys are
        // `verb` or `verb sub`, so match any pending key under this verb.
        let verb_pending = pending_cmd
            .iter()
            .any(|c| c.split(' ').next() == Some(verb) && !top.contains(verb));
        let accepted = ACCEPTED_FLAGS.contains(&(verb, flag));
        let pending = PENDING_FLAGS
            .iter()
            .any(|(c, f, _)| (*c, *f) == (verb, flag));
        if !(verb_pending || accepted || pending) {
            bad.push(format!("`quaqua {pair}` in {in_files:?}"));
        }
    }
    assert!(
        bad.is_empty(),
        "documents name flags no registered command accepts (fix the text, add to ACCEPTED_FLAGS, or list in PENDING_FLAGS with its task):\n{}",
        bad.join("\n")
    );
    let landed: Vec<String> = PENDING_FLAGS
        .iter()
        .filter(|(c, f, _)| ACCEPTED_FLAGS.contains(&(*c, *f)))
        .map(|(c, f, o)| format!("`{c} {f}` ({o})"))
        .collect();
    assert!(
        landed.is_empty(),
        "now accepted; delete from PENDING_FLAGS: {}",
        landed.join(", ")
    );
}

#[test]
fn pending_commands_are_still_missing() {
    let (mut reg, env) = registry();
    let top = registered(&mut reg, &words(PENDING.iter().map(|(c, _)| c.to_string())));
    let built: Vec<String> = PENDING
        .iter()
        .filter(|(c, _)| provided(c, &top, &env) != Ok(false))
        .map(|(c, owner)| format!("`{c}` ({owner})"))
        .collect();
    assert!(
        built.is_empty(),
        "these PENDING commands exist now; delete their entries: {}",
        built.join(", ")
    );
}

#[test]
fn workspace_docs_name_no_removed_features() {
    let mut hits = Vec::new();
    for p in all_files() {
        let text = std::fs::read_to_string(&p).unwrap();
        for (n, line) in text.lines().enumerate() {
            let lower = line.to_ascii_lowercase();
            for w in REMOVED_WORDS {
                let found = lower.match_indices(w).any(|(i, _)| {
                    let before = lower[..i].chars().next_back();
                    let after = lower[i + w.len()..].chars().next();
                    let wc = |c: Option<char>| c.is_some_and(|c| c.is_alphanumeric() || c == '_');
                    !wc(before) && !wc(after)
                });
                if found {
                    hits.push(format!("{}:{}: {w}", rel(&p), n + 1));
                }
            }
        }
    }
    assert!(
        hits.is_empty(),
        "removed names in documents:\n{}",
        hits.join("\n")
    );
    for gone in REMOVED_PATHS {
        assert!(
            !workspace().join(gone).exists(),
            "{gone} must not ship with the new binary"
        );
    }
}

/// The gate blocks `herdr pane send-keys|send-text|run` for every caller, so no
/// shipped document may tell anyone to use them.
#[test]
fn workspace_docs_name_no_blocked_pane_input() {
    let mut hits = Vec::new();
    for p in all_files() {
        let text = std::fs::read_to_string(&p).unwrap();
        for (n, line) in text.lines().enumerate() {
            let w: Vec<&str> = line
                .split(|c: char| c.is_whitespace() || "`\"'()".contains(c))
                .filter(|t| !t.is_empty())
                .collect();
            if w.windows(3).any(|t| {
                t[0] == "herdr"
                    && t[1] == "pane"
                    && matches!(t[2], "send-keys" | "send-text" | "run")
            }) {
                hits.push(format!("{}:{}", rel(&p), n + 1));
            }
        }
    }
    assert!(
        hits.is_empty(),
        "blocked herdr pane input named:\n{}",
        hits.join("\n")
    );
}
