//! Brief contract for `worker prompt` (§ S4): the advisory `Playbook:` and
//! `Context:` headers, the playbook injection appended to the submitted prompt,
//! and the `runs/.<name>.playbook` pin (name + step ids).
//!
//! Everything here warns except one case: when the workspace's playbook index
//! is present, a named playbook it cannot resolve or render is rejected —
//! the worker would otherwise get a prompt naming a playbook whose steps were
//! never delivered. An index that is absent or incomplete cannot decide
//! anything, so it only warns: the brief is not wrong because the workspace is.

use std::path::Path;

use crate::playbook::{
    find_playbook, read_principle, resolve_playbooks_dir, resolve_principles_dir, Playbook,
    PlaybookError,
};

/// What the brief's `Playbook:` header says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlaybookHeader {
    /// No `Playbook:` line at all.
    Missing,
    /// `Playbook: none[, <reason>]` — the unit runs without a playbook.
    None { reason: String },
    /// `Playbook: <name>`.
    Named(String),
    /// `Playbook: <name from …>` — the documented skeleton copied unfilled.
    Placeholder,
}

/// The header block of a brief: the leading lines up to the first blank line.
/// The headers sit there (`rules/_all/brief.md`); everything below is prose,
/// and `Context:` in particular relays arbitrary text (an upstream brief
/// included), so an unbounded scan would let that text decide a header value.
fn header_block(text: &str) -> impl Iterator<Item = &str> {
    text.lines()
        .skip_while(|line| line.trim().is_empty())
        .take_while(|line| !line.trim().is_empty())
}

/// A value still holding the documented `<…>` placeholder, e.g.
/// `Playbook: <name from \`quaqua playbook list\`>` copied out of
/// `rules/_all/brief.md` without being filled in.
fn is_placeholder(value: &str) -> bool {
    value.trim_start().starts_with('<')
}

/// Value of an advisory brief header (`Playbook`, `Context`), matched at the
/// start of a line so prose that mentions the header does not count.
pub fn header_value<'a>(text: &'a str, name: &str) -> Option<&'a str> {
    for line in header_block(text) {
        let Some((key, rest)) = line.trim().split_once(':') else {
            continue;
        };
        if key.trim().eq_ignore_ascii_case(name) {
            return Some(rest.trim());
        }
    }
    None
}

/// `Some(reason)` when the value names `none`; the reason is empty for a bare
/// `none` and for `none,` with nothing after it.
fn none_reason(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Some(String::new());
    }
    let lower = trimmed.to_ascii_lowercase();
    let rest = lower.strip_prefix("none")?;
    if rest.is_empty() {
        return Some(String::new());
    }
    let separator = rest
        .chars()
        .next()
        .is_some_and(|c| c.is_whitespace() || matches!(c, ',' | ';' | ':' | '-' | '—' | '('));
    if !separator {
        // A name that merely starts with `none`, e.g. `nonexistent`.
        return None;
    }
    let reason = trimmed["none".len()..]
        .trim_start_matches(|c: char| {
            c.is_whitespace() || matches!(c, ',' | ';' | ':' | '-' | '—' | '(')
        })
        .trim();
    Some(reason.to_string())
}

/// Parse the brief's `Playbook:` header.
pub fn playbook_header(text: &str) -> PlaybookHeader {
    let Some(value) = header_value(text, "Playbook") else {
        return PlaybookHeader::Missing;
    };
    if is_placeholder(value) {
        return PlaybookHeader::Placeholder;
    }
    if let Some(reason) = none_reason(value) {
        return PlaybookHeader::None { reason };
    }
    match value.split_whitespace().next() {
        Some(name) => PlaybookHeader::Named(name.trim_end_matches([',', ';', ':']).to_string()),
        None => PlaybookHeader::None {
            reason: String::new(),
        },
    }
}

/// The playbook named by the brief, if the header names one (§ S4: the engine
/// never classifies; it only resolves what the coordinator wrote).
pub fn named_playbook(text: &str) -> Option<String> {
    match playbook_header(text) {
        PlaybookHeader::Named(name) => Some(name),
        _ => None,
    }
}

/// Helper to extract both the named playbook and any reason stated on the same line.
/// Returns None if Playbook is missing, none, or placeholder.
/// Returns Some((name, Some(reason))) if a reason is stated after name, or Some((name, None)) if bare name.
pub fn named_playbook_with_reason(text: &str) -> Option<(String, Option<String>)> {
    let value = header_value(text, "Playbook")?;
    if is_placeholder(value) || none_reason(value).is_some() {
        return None;
    }
    let trimmed = value.trim();
    let name_token = trimmed.split_whitespace().next()?;
    let name = name_token.trim_end_matches([',', ';', ':']).to_string();
    if name.is_empty() {
        return None;
    }
    let rest = trimmed[name_token.len()..]
        .trim_start_matches(|c: char| {
            c.is_whitespace() || matches!(c, ',' | ';' | ':' | '-' | '—' | '(')
        })
        .trim();
    let reason = if rest.is_empty() {
        None
    } else {
        Some(rest.to_string())
    };
    Some((name, reason))
}

/// Warnings for the advisory headers. Empty when `Playbook:` and `Context:`
/// are present, a `none` carries a reason, and no `Budget:` line stands in for
/// the machine-read `Time budget:`. Never an error: briefs without them pass as
/// before.
pub fn brief_header_warnings(text: &str) -> Vec<String> {
    let mut warnings = Vec::new();
    match playbook_header(text) {
        PlaybookHeader::Missing => warnings.push(
            "warning: brief has no 'Playbook:' header — add 'Playbook: <name>' from `quaqua playbook list`, or 'Playbook: none, <reason>' for a trivial one-command unit (rules/_all/brief.md)".to_string(),
        ),
        PlaybookHeader::None { reason } if reason.is_empty() => warnings.push(
            "warning: 'Playbook: none' needs a reason: 'Playbook: none, <why this unit has no playbook>'".to_string(),
        ),
        PlaybookHeader::Placeholder => warnings.push(
            "warning: 'Playbook:' still holds the '<…>' placeholder — replace it with a name from `quaqua playbook list`, or 'Playbook: none, <reason>' (rules/_all/brief.md)".to_string(),
        ),
        _ => {}
    }
    match header_value(text, "Context") {
        None => warnings.push(
            "warning: brief has no 'Context:' header — name the files/PRs, or paste the upstream report in full (rules/_all/brief.md)".to_string(),
        ),
        Some(value) if value.is_empty() => warnings.push(
            "warning: 'Context:' is empty; name the files/PRs, or paste the upstream report in full".to_string(),
        ),
        Some(value) if is_placeholder(value) => warnings.push(
            "warning: 'Context:' still holds the '<…>' placeholder — name the files/PRs, or paste the upstream report in full (rules/_all/brief.md)".to_string(),
        ),
        Some(value) if none_reason(value).is_some_and(|r| r.is_empty()) => warnings.push(
            "warning: 'Context: none' needs a reason: 'Context: none, <why nothing upstream matters>'"
                .to_string(),
        ),
        Some(_) => {}
    }
    // `Budget:` is not the machine-read header: only `Time budget:` sets the
    // watchdog allowance, so a brief that writes `Budget:` silently runs on the
    // default. Warn without changing the exit code — the brief still runs.
    if header_value(text, "Budget").is_some() && super::brief_budget_sec(text).is_none() {
        warnings.push(
            "warning: 'Budget:' is not the brief's time header — write 'Time budget: <N> min' (machine-read; rules/_all/brief.md), or the worker falls back to the default budget".to_string(),
        );
    }
    warnings
}

/// Forms a brief can spell the main checkout `root` in: the absolute path,
/// `~/…` and `$HOME/…` when `root` sits under `home`.
fn checkout_spellings(root: &str, home: Option<&str>) -> Vec<String> {
    let root = root.trim_end_matches('/');
    let mut forms = vec![root.to_string()];
    if let Some(rel) = home
        .map(|h| h.trim_end_matches('/'))
        .and_then(|h| root.strip_prefix(h))
        .filter(|r| r.starts_with('/'))
    {
        forms.push(format!("~{rel}"));
        forms.push(format!("$HOME{rel}"));
    }
    forms
}

/// Warn when the brief names the absolute path of the repo's main checkout: a
/// worker that follows it edits the main checkout, not its worktree (case
/// 2026-11-12). Worktree paths (`<root>/../.wt/…`) and sibling dirs sharing the
/// prefix (`quaqua-foo`) do not match; the path must end at a path boundary.
pub fn main_checkout_path_warnings(
    text: &str,
    main_root: &Path,
    home: Option<&Path>,
) -> Vec<String> {
    let (Some(root), home) = (main_root.to_str(), home.and_then(Path::to_str)) else {
        return Vec::new();
    };
    let is_path_char = |c: char| c.is_alphanumeric() || matches!(c, '_' | '-' | '.' | '/');
    for form in checkout_spellings(root, home) {
        for (at, _) in text.match_indices(&form) {
            let before = text[..at].chars().next_back();
            let mut rest = text[at + form.len()..].chars();
            let after = rest.next();
            // A '.' ends the path only as sentence punctuation (`…quaqua.`).
            let ends_clean = match after {
                None => true,
                Some('.') => rest.next().is_none_or(|c| !is_path_char(c)),
                Some(c) => c == '/' || !is_path_char(c),
            };
            let starts_clean = before.is_none_or(|c| !is_path_char(c));
            if starts_clean && ends_clean {
                return vec![format!(
                    "warning: brief names the main checkout path '{form}' — a worker may edit it instead of its worktree; use worktree-relative paths (e.g. `crates/…`)"
                )];
            }
        }
    }
    Vec::new()
}

/// Main checkout of the repo containing `cwd` (the parent of the shared git
/// dir), or `None` outside a repo.
fn main_checkout_root(cwd: &Path) -> Option<std::path::PathBuf> {
    let out = std::process::Command::new("git")
        .args(["rev-parse", "--path-format=absolute", "--git-common-dir"])
        .current_dir(cwd)
        .output()
        .ok()
        .filter(|o| o.status.success())?;
    let dir = std::path::PathBuf::from(String::from_utf8(out.stdout).ok()?.trim());
    dir.parent().map(Path::to_path_buf)
}

/// Print the main-checkout path warning for a brief on stderr.
pub fn warn_main_checkout_path(task: &str) {
    let Some(root) = std::env::current_dir()
        .ok()
        .and_then(|cwd| main_checkout_root(&cwd))
    else {
        return;
    };
    let home = std::env::var_os("HOME").map(std::path::PathBuf::from);
    for warning in main_checkout_path_warnings(task, &root, home.as_deref()) {
        eprintln!("{warning}");
    }
}

/// The text appended to a submitted prompt for one playbook: the `## Worker`
/// steps, each cited leaf once, and the pre-filled `## Playbook` block. The
/// `## Orchestrator` text is never included — that is the coordinator's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlaybookInjection {
    /// Playbook name from the file's frontmatter.
    pub name: String,
    /// Step ids the worker was given, in order.
    pub ids: Vec<u32>,
    /// The block appended to the brief, starting with a newline.
    pub text: String,
}

impl PlaybookInjection {
    /// Content of `runs/.<name>.playbook`: the name and the step ids, so a
    /// playbook edited between prompt and report still checks against the ids
    /// the worker actually received.
    pub fn state_content(&self) -> String {
        let ids: Vec<String> = self.ids.iter().map(|id| id.to_string()).collect();
        format!("name={}\nids={}\n", self.name, ids.join(","))
    }
}

/// Render the injection for an already-parsed playbook, reading each cited
/// leaf once from `principles_dir`.
pub fn render_injection(
    playbook: &Playbook,
    principles_dir: &Path,
) -> Result<PlaybookInjection, PlaybookError> {
    let ids = playbook.step_ids();
    let mut text = format!(
        "\n## Worker (playbook: {})\n\nReport every step in your report's `## Playbook` block: `<id>: done`, or `<id>: skip: <reason>`.\n",
        playbook.name
    );
    for step in &playbook.steps {
        text.push_str(&format!("\n{}. {}", step.id, step.text));
    }
    text.push('\n');
    for citation in &playbook.cited_principles {
        let leaf = read_principle(citation, principles_dir)?;
        text.push_str(&format!("\n### {}\n\n{}\n", citation, leaf.trim()));
    }
    text.push_str("\n## Playbook\n");
    for id in &ids {
        text.push_str(&format!("{id}: \n"));
    }
    Ok(PlaybookInjection {
        name: playbook.name.clone(),
        ids,
        text,
    })
}

/// Resolve the brief's playbook in an explicit index and build the injection.
/// `None` when the brief names no playbook. A name the index cannot resolve
/// (unknown, unreadable file, citation it cannot read) is rejected with the
/// brief-error exit code: the submitted prompt would otherwise promise steps
/// the worker never got. An index that is absent or unreadable is not the
/// brief's fault and is left to [`prepare_injection_in_workspace`] to
/// downgrade.
pub fn prepare_injection(
    task: &str,
    playbooks_dir: &Path,
    principles_dir: &Path,
) -> Result<Option<PlaybookInjection>, (i32, String)> {
    let Some(name) = named_playbook(task) else {
        return Ok(None);
    };
    let playbook = find_playbook(&name, playbooks_dir, principles_dir).map_err(|e| match e {
        PlaybookError::NotFound(_) => (
            1,
            format!("error: unknown playbook '{name}' (run `quaqua playbook list`; rules/_all/brief.md)\n"),
        ),
        other => (
            1,
            format!("error: cannot load playbook '{name}': {other}\n"),
        ),
    })?;
    let injection = render_injection(&playbook, principles_dir)
        .map_err(|e| (1, format!("error: cannot render playbook '{name}': {e}\n")))?;
    Ok(Some(injection))
}

/// [`prepare_injection`] against the workspace's `playbooks/` and
/// `principles/`. An unresolvable or missing index only warns: the brief is
/// not wrong because the workspace is.
pub fn prepare_injection_in_workspace(
    task: &str,
) -> Result<Option<PlaybookInjection>, (i32, String)> {
    if named_playbook(task).is_none() {
        return Ok(None);
    }
    let (playbooks_dir, principles_dir) =
        match (resolve_playbooks_dir(None), resolve_principles_dir(None)) {
            (Ok(playbooks), Ok(principles)) => (playbooks, principles),
            (Err(e), _) | (_, Err(e)) => {
                eprintln!(
                    "warning: cannot resolve the playbook index ({e}); 'Playbook:' not checked"
                );
                return Ok(None);
            }
        };
    // Both directories are required: with either one absent no playbook can
    // be resolved at all, so a name that looks unknown may just be unresolvable
    // — the whole header is left unchecked (warn) rather than failing every
    // named brief in a workspace whose index is incomplete.
    if !playbooks_dir.is_dir() || !principles_dir.is_dir() {
        eprintln!(
            "warning: playbook index missing ({} / {}); 'Playbook:' not checked",
            playbooks_dir.display(),
            principles_dir.display()
        );
        return Ok(None);
    }
    prepare_injection(task, &playbooks_dir, &principles_dir)
}

/// Print the advisory header warnings for a brief on stderr.
pub fn warn_brief_headers(task: &str) {
    for warning in brief_header_warnings(task) {
        eprintln!("{warning}");
    }
}

/// Read the Lead's pinned playbook name from `runs/.<lead_name>.playbook`.
pub fn read_lead_playbook(runs_dir: &Path, lead_name: &str) -> Option<String> {
    let path = runs_dir.join(format!(".{lead_name}.playbook"));
    let text = std::fs::read_to_string(path).ok()?;
    for line in text.lines() {
        if let Some(value) = line.trim().strip_prefix("name=") {
            let n = value.trim();
            if !n.is_empty() {
                return Some(n.to_string());
            }
        }
    }
    None
}

/// Warnings for Lead routing per Peer (templates/workspace/AGENTS.md '## Coordinator Rules' — Route).
/// Flags a Peer brief whose `Playbook:` equals the Lead's own pinned playbook
/// (from `runs/.<owner>.playbook` if present) without a stated reason.
pub fn lead_routing_warnings(text: &str, owner: Option<&str>, runs_dir: &Path) -> Vec<String> {
    let Some(owner) = owner else {
        return Vec::new();
    };
    if !owner.ends_with("-lead") {
        return Vec::new();
    }
    let mut warnings = Vec::new();
    if let Some(lead_pb) = read_lead_playbook(runs_dir, owner) {
        if let Some((name, reason)) = named_playbook_with_reason(text) {
            if name.eq_ignore_ascii_case(&lead_pb) && reason.is_none() {
                warnings.push(format!(
                    "warning: brief uses the Lead's own playbook '{name}' without a stated reason (AGENTS.md '## Coordinator Rules' — Route)"
                ));
            }
        }
    }
    warnings
}

/// Print the lead routing warnings for a brief on stderr.
pub fn warn_lead_routing(text: &str, owner: Option<&str>, runs_dir: &Path) {
    for warning in lead_routing_warnings(text, owner, runs_dir) {
        eprintln!("{warning}");
    }
}

/// § S4: write (or drop) `runs/.<name>.playbook` for the prompt that just
/// landed. `None` removes the file, so a pin left by an earlier task can never
/// be checked against this task's report. A temp file + rename keeps a failed
/// write from leaving a partial pin. The one writer of the pin format, shared
/// by `worker prompt` and `quaqua pipeline`.
pub fn pin_playbook(runs_dir: &Path, name: &str, injection: Option<&PlaybookInjection>) {
    let path = runs_dir.join(format!(".{name}.playbook"));
    let Some(injection) = injection else {
        let _ = std::fs::remove_file(&path);
        return;
    };
    let tmp = runs_dir.join(format!(".{name}.playbook.tmp"));
    let written =
        std::fs::write(&tmp, injection.state_content()).and_then(|()| std::fs::rename(&tmp, &path));
    if let Err(e) = written {
        let _ = std::fs::remove_file(&tmp);
        eprintln!("warning: cannot write {}: {e}", path.display());
    }
}

/// The prompt body handed to the agent: the brief, the injection when there is
/// one, then the report instruction. Without an injection it is byte-identical
/// to the body before § S4.
pub fn compose_prompt_body(name: &str, task: &str, injection: Option<&str>) -> String {
    let tail = format!(
        "When done, write summary (changes, test cmd+result, risks) to a temp file and rename it to ~/quaqua/runs/{name}.md (publish it atomically, never a partial file); reply only the path."
    );
    match injection {
        Some(block) => format!("{task}\n{block}\n{tail}"),
        None => format!("{task}. {tail}"),
    }
}

/// Review severity, lowest to highest (#178). The order is the contract: a
/// finding blocks acceptance when it is at or above the brief's `Must fix:`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    Trivial,
    Minor,
    Major,
    Critical,
}

impl Severity {
    pub const ALL: [Severity; 4] = [
        Severity::Trivial,
        Severity::Minor,
        Severity::Major,
        Severity::Critical,
    ];

    pub fn parse(value: &str) -> Option<Severity> {
        Severity::ALL
            .into_iter()
            .find(|s| s.as_str().eq_ignore_ascii_case(value.trim()))
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Severity::Trivial => "trivial",
            Severity::Minor => "minor",
            Severity::Major => "major",
            Severity::Critical => "critical",
        }
    }
}

/// Default `Must fix:` threshold when the brief does not state one.
pub const DEFAULT_MUST_FIX: Severity = Severity::Major;

/// The review rule a brief states: `Must fix: <severity>` and
/// `Review rounds: <N>`. Both optional; `None` means "not stated".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReviewPolicy {
    pub must_fix: Option<Severity>,
    pub rounds: Option<u32>,
}

impl ReviewPolicy {
    pub fn must_fix_or_default(&self) -> Severity {
        self.must_fix.unwrap_or(DEFAULT_MUST_FIX)
    }
}

/// Parse the brief's review headers. A stated but invalid value is an error
/// (exit 1 at `--check-brief`); an absent one is not.
pub fn review_policy(text: &str) -> Result<ReviewPolicy, String> {
    let must_fix = match header_value(text, "Must fix") {
        None => None,
        Some(v) => Some(Severity::parse(v).ok_or_else(|| {
            format!(
                "error: 'Must fix: {v}' is not a severity — use one of critical, major, minor, trivial (rules/_all/brief.md)\n"
            )
        })?),
    };
    let rounds = match header_value(text, "Review rounds") {
        None => None,
        Some(v) => match v.trim().parse::<u32>() {
            Ok(n) if n >= 1 => Some(n),
            _ => {
                return Err(format!(
                    "error: 'Review rounds: {v}' must be an integer >= 1 (rules/_all/brief.md)\n"
                ))
            }
        },
    };
    Ok(ReviewPolicy { must_fix, rounds })
}

/// Severity of a reviewer finding line `- [<severity>] <finding>` (also `*`
/// bullets and `1.` numbering). `None` for any other line.
pub fn finding_severity(line: &str) -> Option<Severity> {
    let t = line.trim_start();
    let rest = if let Some(r) = t.strip_prefix("- ").or_else(|| t.strip_prefix("* ")) {
        r
    } else {
        let digits = t.chars().take_while(|c| c.is_ascii_digit()).count();
        if digits == 0 {
            return None;
        }
        t[digits..].strip_prefix(". ")?
    };
    let inner = rest.trim_start().strip_prefix('[')?;
    let (tag, _) = inner.split_once(']')?;
    Severity::parse(tag)
}

/// Lines of the report's `## Findings` section (the whole report when it has
/// no such heading), up to the next `## ` heading or the verdict line.
fn findings_section(report: &str) -> Vec<&str> {
    let has_heading = report.lines().any(|l| l.trim() == "## Findings");
    let mut inside = !has_heading;
    let mut out = Vec::new();
    for line in report.lines() {
        let t = line.trim();
        if t == "## Findings" {
            inside = true;
            continue;
        }
        if t.to_ascii_lowercase().starts_with("verdict:") || (has_heading && t.starts_with("## ")) {
            inside = false;
            continue;
        }
        if inside {
            out.push(line);
        }
    }
    out
}

/// Findings in a review report that block acceptance under `must_fix`:
/// those tagged at or above it, plus every finding of `Verdict: <N>` that
/// carries no valid tag (`N - tagged`, fail closed).
pub fn blocking_findings(report: &str, must_fix: Severity, verdict_count: u32) -> u32 {
    let tagged: Vec<Severity> = findings_section(report)
        .into_iter()
        .filter_map(finding_severity)
        .collect();
    let at_or_above = tagged.iter().filter(|s| **s >= must_fix).count() as u32;
    at_or_above + verdict_count.saturating_sub(tagged.len() as u32)
}

/// Tagged findings below `must_fix`: recorded, not fixed.
pub fn below_threshold_findings(report: &str, must_fix: Severity) -> Vec<String> {
    findings_section(report)
        .into_iter()
        .filter(|l| finding_severity(l).is_some_and(|s| s < must_fix))
        .map(|l| l.trim().to_string())
        .collect()
}

#[cfg(test)]
mod review_severity_tests {
    use super::*;

    const BASE: &str =
        "Topology: solo — x\nGoal: g\nHard constraints: h\nCurrent design (open to challenge): d\n";

    #[test]
    fn review_policy_reads_stated_fields_and_defaults_when_absent() {
        let p = review_policy(&format!("{BASE}Must fix: minor\nReview rounds: 3\n")).unwrap();
        assert_eq!(p.must_fix, Some(Severity::Minor));
        assert_eq!(p.rounds, Some(3));
        let p = review_policy(BASE).unwrap();
        assert_eq!(
            p,
            ReviewPolicy {
                must_fix: None,
                rounds: None
            }
        );
        assert_eq!(p.must_fix_or_default(), Severity::Major);
    }

    #[test]
    fn review_policy_rejects_bad_values() {
        assert!(review_policy(&format!("{BASE}Must fix: severe\n")).is_err());
        assert!(review_policy(&format!("{BASE}Review rounds: 0\n")).is_err());
        assert!(review_policy(&format!("{BASE}Review rounds: two\n")).is_err());
    }

    #[test]
    fn blocking_findings_counts_at_or_above_threshold() {
        let report = "## Findings\n- [critical] a\n- [major] b\n1. [minor] c\n* [trivial] d\nVerdict: 4 findings\n";
        assert_eq!(blocking_findings(report, Severity::Major, 4), 2);
        assert_eq!(blocking_findings(report, Severity::Critical, 4), 1);
        assert_eq!(blocking_findings(report, Severity::Trivial, 4), 4);
        let minor_only = "- [minor] a\n- [trivial] b\nVerdict: 2 findings\n";
        assert_eq!(blocking_findings(minor_only, Severity::Major, 2), 0);
        assert_eq!(
            below_threshold_findings(minor_only, Severity::Major),
            vec!["- [minor] a", "- [trivial] b"]
        );
        // Untagged review: every verdict finding blocks.
        assert_eq!(
            blocking_findings("1. a\nVerdict: 1 findings\n", Severity::Major, 1),
            1
        );
        // Malformed tags count as untagged.
        let bad = "## Findings\n- **[major]** x\n### [major] y\n- [blocker] z\n- [trivial] t\nVerdict: 4 findings\n";
        assert_eq!(blocking_findings(bad, Severity::Major, 4), 3);
        // Tags outside `## Findings` do not count against a clean verdict.
        let clean = "## Resolved\n- [major] X — fixed\n## Findings\nnone\nVerdict: clean\n";
        assert_eq!(blocking_findings(clean, Severity::Major, 0), 0);
    }
}

#[cfg(test)]
mod main_checkout_path_tests {
    use super::*;

    const ROOT: &str = "/h/u/Lab/quaqua";
    const HOME: &str = "/h/u";

    fn warns(text: &str) -> bool {
        !main_checkout_path_warnings(text, Path::new(ROOT), Some(Path::new(HOME))).is_empty()
    }

    #[test]
    fn warns_on_absolute_tilde_and_home_spellings() {
        assert!(warns("edit /h/u/Lab/quaqua/crates/x.rs please"));
        assert!(warns("Context: ~/Lab/quaqua/crates"));
        assert!(warns("see $HOME/Lab/quaqua/src"));
        assert!(warns("repo is /h/u/Lab/quaqua."));
        assert!(warns("(~/Lab/quaqua)"));
    }

    #[test]
    fn no_warning_for_worktree_siblings_and_relative_paths() {
        assert!(!warns("edit /h/u/Lab/.wt/sude-x/crates/x.rs"));
        assert!(!warns("edit ~/Lab/.wt/sude-x/crates/x.rs"));
        assert!(!warns("see /h/u/Lab/quaqua-other/src and ~/Lab/quaquax"));
        assert!(!warns("see /other/h/u/Lab/quaqua/src"));
        assert!(!warns("edit crates/quaqua/src/main.rs"));
        assert!(!warns("Goal: g\nHard constraints: crates/ only"));
    }
}
