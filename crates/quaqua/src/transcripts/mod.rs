//! `quaqua transcripts` — recall over past Claude Code sessions.
//!
//! The CoS keeps `memory/rules.md` and dated logs, but a decision made three
//! weeks ago usually survives only in a session transcript under
//! `~/.claude/projects/<project>/<session>.jsonl`. This module makes that
//! corpus searchable without an LLM in the loop:
//!
//! - `index` reads `<projects>/*/*.jsonl` into `runs/transcripts.db`, an FTS5
//!   table (`tokenize = 'trigram'`, so any substring of three characters or
//!   more matches, case-insensitively). It is incremental by `(mtime, size)`,
//!   so a second run over an unchanged corpus inserts nothing.
//! - `search` prints `path:line  <ts> <role>  <excerpt>`, best `bm25` first.
//! - `tail` prints the last N user messages of one session, straight from the
//!   JSONL — no index needed, which is what the pre-compaction snapshot uses.
//!
//! The corpus is read-only and the index is its own file: this module never
//! opens, creates or writes the bus database (`runs/quaqua.db`).

mod db;
mod parse;

use std::path::{Path, PathBuf};

use db::{Hit, Query};

/// Longest excerpt `search` prints, in chars (R5).
pub const EXCERPT_CHARS: usize = 200;
/// Hits per search when `--limit` is not given.
pub const DEFAULT_LIMIT: usize = 20;
/// Messages `tail` prints when `--last` is not given.
pub const DEFAULT_LAST: usize = 10;

/// `quaqua transcripts index` options.
#[derive(Debug, Clone, Default)]
pub struct IndexOptions {
    /// Corpus root; defaults to `~/.claude/projects`.
    pub projects: Option<PathBuf>,
    /// Index file; defaults to `<runs>/transcripts.db`.
    pub db: Option<PathBuf>,
}

/// `quaqua transcripts search` options.
#[derive(Debug, Clone, Default)]
pub struct SearchOptions {
    pub query: String,
    pub limit: Option<usize>,
    pub project: Option<String>,
    pub since: Option<String>,
    pub projects: Option<PathBuf>,
    pub db: Option<PathBuf>,
}

/// `quaqua transcripts tail` options.
#[derive(Debug, Clone, Default)]
pub struct TailOptions {
    /// Session id or path to a `.jsonl` file.
    pub session: String,
    pub last: Option<usize>,
    pub projects: Option<PathBuf>,
}

/// What one `index` run did.
#[derive(Debug, Clone, Default)]
pub struct Summary {
    pub total: usize,
    pub changed: usize,
    pub unchanged: usize,
    pub rows: usize,
    pub pruned: usize,
    pub unparseable: u64,
}

/// The corpus root: `~/.claude/projects` unless `--projects` overrides it.
pub fn projects_root(override_dir: Option<&Path>) -> PathBuf {
    if let Some(p) = override_dir {
        return p.to_path_buf();
    }
    match std::env::var_os("HOME") {
        Some(home) => PathBuf::from(home).join(".claude").join("projects"),
        None => PathBuf::from(".claude").join("projects"),
    }
}

/// The index file: `<runs>/transcripts.db` unless `--db` overrides it.
pub fn db_path(override_db: Option<&Path>) -> PathBuf {
    match override_db {
        Some(p) => p.to_path_buf(),
        None => crate::worker::cleanup::default_runs_dir().join("transcripts.db"),
    }
}

/// Every `<root>/<project>/<session>.jsonl`, sorted, with its project name.
///
/// One level deep on purpose: `<root>/<project>/<session>/subagents/*.jsonl`
/// holds subagent transcripts, which are not the CoS's own conversation.
fn corpus_files(root: &Path) -> std::io::Result<Vec<(String, PathBuf)>> {
    let mut out = Vec::new();
    for dir in std::fs::read_dir(root)? {
        let dir = dir?;
        if !dir.file_type()?.is_dir() {
            continue;
        }
        let project = dir.file_name().to_string_lossy().into_owned();
        for file in std::fs::read_dir(dir.path())? {
            let file = file?;
            let path = file.path();
            if path.extension().map(|e| e == "jsonl").unwrap_or(false) {
                out.push((project.clone(), path));
            }
        }
    }
    out.sort_by(|a, b| a.1.cmp(&b.1));
    Ok(out)
}

/// Builds or refreshes the index. Idempotent: an unchanged corpus writes no rows.
pub fn index_corpus(projects_root: &Path, db_file: &Path) -> Result<Summary, String> {
    if !projects_root.is_dir() {
        return Err(format!(
            "transcripts: {} is not a directory",
            projects_root.display()
        ));
    }
    let files = corpus_files(projects_root)
        .map_err(|e| format!("transcripts: reading {}: {e}", projects_root.display()))?;
    let mut conn = db::open(db_file)
        .map_err(|e| format!("transcripts: opening {}: {e}", db_file.display()))?;

    let mut summary = Summary {
        total: files.len(),
        ..Summary::default()
    };
    let mut keep = std::collections::HashSet::with_capacity(files.len());
    for (project, path) in &files {
        let key = path.to_string_lossy().into_owned();
        keep.insert(key.clone());
        let meta = std::fs::metadata(path)
            .map_err(|e| format!("transcripts: stat {}: {e}", path.display()))?;
        let stamp = db::stamp_of(&meta);
        if db::stored_stamp(&conn, &key)
            .map_err(|e| format!("transcripts: reading {}: {e}", db_file.display()))?
            == Some(stamp)
        {
            summary.unchanged += 1;
            continue;
        }
        let scan = parse::read_entries(path)
            .map_err(|e| format!("transcripts: reading {}: {e}", path.display()))?;
        summary.unparseable += scan.unparseable;
        summary.rows += db::index_file(&mut conn, &key, project, &scan.entries, stamp)
            .map_err(|e| format!("transcripts: indexing {}: {e}", path.display()))?;
        summary.changed += 1;
    }
    summary.pruned = db::prune(&mut conn, &keep)
        .map_err(|e| format!("transcripts: pruning {}: {e}", db_file.display()))?;
    Ok(summary)
}

/// `quaqua transcripts index`.
pub fn run_index(opts: &IndexOptions) -> i32 {
    let root = projects_root(opts.projects.as_deref());
    let db_file = db_path(opts.db.as_deref());
    match index_corpus(&root, &db_file) {
        Ok(s) => {
            println!(
                "indexed {}/{} files, {} rows ({} unchanged) -> {}",
                s.changed,
                s.total,
                s.rows,
                s.unchanged,
                db_file.display()
            );
            if s.unparseable > 0 {
                eprintln!("transcripts: skipped {} unparseable lines", s.unparseable);
            }
            0
        }
        Err(e) => {
            eprintln!("{e}");
            1
        }
    }
}

/// `quaqua transcripts search <query>`.
pub fn run_search(opts: &SearchOptions) -> i32 {
    let query_text = opts.query.trim();
    if query_text.is_empty() {
        eprintln!("transcripts search: empty query");
        return 1;
    }
    let root = projects_root(opts.projects.as_deref());
    let db_file = db_path(opts.db.as_deref());
    if !db_file.is_file() {
        // R5: a missing index is built once before the first search. The
        // summary goes to stderr so stdout stays one line per hit.
        match index_corpus(&root, &db_file) {
            Ok(s) => eprintln!(
                "transcripts: built {} ({} rows from {} files)",
                db_file.display(),
                s.rows,
                s.total
            ),
            Err(e) => {
                eprintln!("{e}");
                return 1;
            }
        }
    }
    let conn = match db::open(&db_file) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("transcripts: opening {}: {e}", db_file.display());
            return 1;
        }
    };
    let query = build_query(
        query_text,
        opts.limit.unwrap_or(DEFAULT_LIMIT),
        opts.project.clone(),
        opts.since.clone(),
    );
    let hits = match db::search(&conn, &query) {
        Ok(h) => h,
        Err(e) => {
            eprintln!("transcripts: searching {}: {e}", db_file.display());
            return 1;
        }
    };
    if hits.is_empty() {
        println!("no matches");
        return 0;
    }
    for hit in &hits {
        println!("{}", render_hit(hit));
    }
    0
}

/// `quaqua transcripts tail --session <id|path> --last N`.
pub fn run_tail(opts: &TailOptions) -> i32 {
    let root = projects_root(opts.projects.as_deref());
    let Some(path) = resolve_session(&opts.session, &root) else {
        eprintln!("transcripts tail: unknown session {}", opts.session);
        return 1;
    };
    let scan = match parse::read_entries(&path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("transcripts tail: reading {}: {e}", path.display());
            return 1;
        }
    };
    let last = opts.last.unwrap_or(DEFAULT_LAST);
    let prompts: Vec<&parse::Entry> = scan
        .entries
        .iter()
        .filter(|e| e.role == "user" && !e.sidechain)
        .collect();
    let start = prompts.len().saturating_sub(last);
    for entry in &prompts[start..] {
        println!("{}", collapse(&entry.text));
    }
    0
}

/// Resolves `--session`: a path when it looks like one, else `<root>/*/<id>.jsonl`.
fn resolve_session(arg: &str, root: &Path) -> Option<PathBuf> {
    let as_path = PathBuf::from(arg);
    if arg.contains('/') || arg.ends_with(".jsonl") {
        return as_path.is_file().then_some(as_path);
    }
    if as_path.is_file() {
        return Some(as_path);
    }
    corpus_files(root)
        .ok()?
        .into_iter()
        .map(|(_, p)| p)
        .find(|p| p.file_stem().and_then(|s| s.to_str()) == Some(arg))
}

/// Turns a query into the `MATCH` expression plus the `LIKE` terms trigram
/// cannot serve (a term under three characters matches no trigram at all).
fn build_query(query: &str, limit: usize, project: Option<String>, since: Option<String>) -> Query {
    let mut match_terms = Vec::new();
    let mut like_terms = Vec::new();
    for term in query.split_whitespace() {
        if term.chars().count() >= 3 {
            match_terms.push(format!("\"{}\"", term.replace('"', "\"\"")));
        } else {
            like_terms.push(term.to_string());
        }
    }
    let match_expr = if match_terms.is_empty() {
        None
    } else {
        Some(match_terms.join(" AND "))
    };
    if match_expr.is_none() && like_terms.is_empty() {
        // Whitespace-only input never reaches here (run_search rejects it);
        // this keeps the scan path honest if it ever does.
        like_terms.push(query.to_string());
    }
    Query {
        match_expr,
        like_terms,
        project,
        since,
        limit,
    }
}

/// One line per hit: `path:line  <ts> <role>  <excerpt>` (R5).
fn render_hit(hit: &Hit) -> String {
    let ts = if hit.ts.is_empty() {
        "-"
    } else {
        hit.ts.as_str()
    };
    format!(
        "{}:{}  {} {}  {}",
        hit.path,
        hit.line,
        ts,
        hit.role,
        excerpt(&hit.text)
    )
}

/// Whitespace runs collapsed to one space, cut to [`EXCERPT_CHARS`] chars.
fn excerpt(text: &str) -> String {
    let flat = collapse(text);
    if flat.chars().count() <= EXCERPT_CHARS {
        flat
    } else {
        flat.chars().take(EXCERPT_CHARS).collect()
    }
}

/// Whitespace runs collapsed to one space.
fn collapse(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for word in text.split_whitespace() {
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(word);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_query_splits_into_trigram_terms_and_like_terms() {
        let q = build_query("memory caps ab", 5, None, None);
        assert_eq!(q.match_expr.as_deref(), Some("\"memory\" AND \"caps\""));
        assert_eq!(q.like_terms, vec!["ab".to_string()]);
        assert_eq!(q.limit, 5);
    }

    #[test]
    fn a_quote_in_a_term_is_escaped_for_match() {
        let q = build_query("say \"hi\"", 5, None, None);
        assert_eq!(q.match_expr.as_deref(), Some("\"say\" AND \"\"\"hi\"\"\""));
    }

    #[test]
    fn a_query_with_only_short_terms_scans() {
        let q = build_query("ab cd", 5, None, None);
        assert!(q.match_expr.is_none());
        assert_eq!(q.like_terms, vec!["ab".to_string(), "cd".to_string()]);
    }

    #[test]
    fn an_excerpt_is_collapsed_and_cut_at_two_hundred_chars() {
        let long = "x".repeat(500);
        assert_eq!(excerpt(&long).chars().count(), EXCERPT_CHARS);
        assert_eq!(excerpt("a\n\n  b\tc"), "a b c");
        // Char-, not byte-, counted: 200 Vietnamese chars stay whole.
        let vi = "ắ".repeat(300);
        assert_eq!(excerpt(&vi).chars().count(), EXCERPT_CHARS);
    }

    #[test]
    fn a_hit_renders_the_anchor_then_ts_role_then_excerpt() {
        let hit = Hit {
            path: "/p/a.jsonl".to_string(),
            line: "7".to_string(),
            ts: "2026-11-01T10:00:00Z".to_string(),
            role: "user".to_string(),
            text: "memory  caps\nare fine".to_string(),
        };
        assert_eq!(
            render_hit(&hit),
            "/p/a.jsonl:7  2026-11-01T10:00:00Z user  memory caps are fine"
        );
    }
}
