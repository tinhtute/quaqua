//! The report contract, defined once: what a report must carry for the engine
//! to machine-read it. The briefs' asks and the validators are all derived
//! from the values here, so an ask and its check cannot drift.
//!
//! - Reviewer report: a `Verdict:` line (any position; the last one wins),
//!   `clean` or `<N> findings`. Review requires it.
//! - Builder report: an `Outcome:` field at column 1, read by the worker
//!   judge (`Verified:` is then judged on its own, with a named failure).
//! - A plan or lessons-only report is accepted by its `## Lessons` heading.
//! - `## Playbook`, `## Blockers` and any other section are not machine-read
//!   by the engine, so a report is never malformed for lacking or misplacing
//!   them.

/// The verdict field a reviewer report ends with.
pub(crate) const VERDICT_FIELD: &str = "Verdict:";
/// The outcome field a builder report starts with.
pub(crate) const OUTCOME_FIELD: &str = "Outcome:";
/// The heading of a plan or lessons-only report.
pub(crate) const LESSONS_HEADING: &str = "Lessons";
/// Every `Outcome:` value the engine parses in a builder report
/// (`worker_flow::judge`, `pipeline::unfinished`); the skeleton lists exactly these.
pub(crate) const OUTCOME_VALUES: [&str; 6] = [
    "COMPLETE",
    "NO_CHANGES",
    "BLOCKED",
    "DISPUTED",
    "REOPEN_REQUEST",
    "DEPENDENCY_REQUEST",
];
/// The two verdict values the reviewer may write.
pub(crate) const VERDICT_CLEAN: &str = "clean";
pub(crate) const VERDICT_FINDINGS: &str = "findings";

/// The verdict line a reviewer brief asks for.
pub(crate) fn verdict_ask() -> String {
    format!(
        "End with exactly one line: '{VERDICT_FIELD} {VERDICT_CLEAN}' or '{VERDICT_FIELD} <N> {VERDICT_FINDINGS}'"
    )
}

/// The report skeleton a worker brief asks for. Rendered from the fields the
/// gate (`well_formed`) checks, so the ask and the check cannot drift.
pub(crate) fn report_skeleton() -> String {
    format!(
        "{OUTCOME_FIELD} {}   (first line, column 1)\n\
         ## Changes\n<what changed>\n\
         ## Verify\n<test command + result>\n\
         ## Risks\n<risks, or none>\n\
         ## {LESSONS_HEADING}\n<at most 2 lines, or none>   (last heading)",
        OUTCOME_VALUES.join(" | ")
    )
}

/// The report instruction appended to a worker brief.
pub(crate) fn report_ask(name: &str) -> String {
    format!(
        "When done, write a report shaped exactly like this skeleton to a temp file and rename it to ~/quaqua/runs/{name}.md (publish it atomically, never a partial file); reply only the path.\n{}",
        report_skeleton()
    )
}

/// The last `Verdict:` line of a report, trimmed. `Verdict:` or `verdict:`, as before.
pub(crate) fn verdict_line(text: &str) -> Option<&str> {
    let lower = VERDICT_FIELD.to_ascii_lowercase();
    text.lines()
        .map(str::trim)
        .rfind(|l| l.starts_with(VERDICT_FIELD) || l.starts_with(&lower))
}

/// The heading under which a report disputes a premise or finding.
pub(crate) const CHALLENGE_HEADING: &str = "## Challenge";
/// Opening sentences that say a Challenge section disputes nothing.
const NO_DISPUTE: [&str; 7] = [
    "none",
    "n/a",
    "nothing",
    "no challenge",
    "no challenges",
    "no dispute",
    "nothing disputed",
];

/// True when the report disputes something: it has a `## Challenge` section
/// whose body is not empty and does not open with a no-dispute statement
/// ("None.", "N/A"). Fails closed: any other body, including "None of the
/// premises hold: ...", is a dispute. Shared by the pipeline and the auditor.
pub fn disputes(text: &str) -> bool {
    let mut lines = text.lines();
    while let Some(l) = lines.next() {
        if !l.trim_start().starts_with(CHALLENGE_HEADING) {
            continue;
        }
        let body: Vec<&str> = lines
            .clone()
            .take_while(|l| !l.trim_start().starts_with('#'))
            .collect();
        let body = body.join("\n");
        let body = body.trim();
        if body.is_empty() {
            continue;
        }
        let first = body
            .split(|c| matches!(c, '.' | '!' | '\n'))
            .next()
            .unwrap_or("")
            .trim_matches(|c: char| !c.is_alphanumeric() && c != '/')
            .to_ascii_lowercase();
        if !NO_DISPUTE.contains(&first.as_str()) {
            return true;
        }
    }
    false
}

fn has_outcome(text: &str) -> bool {
    text.lines().any(|l| l.starts_with(OUTCOME_FIELD))
}

fn has_lessons(text: &str) -> bool {
    text.lines()
        .any(|l| l.starts_with("## ") && l.contains(LESSONS_HEADING))
}

/// A report the engine can machine-read: it carries a verdict, an outcome, or
/// a lessons heading. Prose with none of them is malformed.
pub fn well_formed(text: &str) -> bool {
    verdict_line(text).is_some() || has_outcome(text) || has_lessons(text)
}

/// `Verdict: clean` or `Verdict: <N> findings`; the last verdict line wins and
/// anything else fails closed.
pub(crate) fn parse_verdict(text: &str) -> Result<u32, String> {
    let line = verdict_line(text).ok_or_else(|| {
        format!(
            "review report has no '{VERDICT_FIELD} {VERDICT_CLEAN} | <N> {VERDICT_FINDINGS}' line"
        )
    })?;
    let rest = line.split_once(':').map(|(_, r)| r.trim()).unwrap_or("");
    if rest.eq_ignore_ascii_case(VERDICT_CLEAN) {
        return Ok(0);
    }
    let mut w = rest.split_whitespace();
    if let (Some(n), Some(noun), None) = (w.next(), w.next(), w.next()) {
        let noun = noun.trim_matches(|c: char| !c.is_alphanumeric());
        let singular = VERDICT_FINDINGS.trim_end_matches('s');
        if let Ok(n) = n.parse::<u32>() {
            if noun.eq_ignore_ascii_case(VERDICT_FINDINGS) || noun.eq_ignore_ascii_case(singular) {
                return Ok(n);
            }
        }
    }
    Err(format!("unparseable verdict line '{line}'"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ask_is_rendered_from_the_contract_fields() {
        let ask = verdict_ask();
        assert!(ask.contains("'Verdict: clean'") && ask.contains("'Verdict: <N> findings'"));
        // whatever the ask names, the parser reads
        assert_eq!(parse_verdict("Verdict: clean"), Ok(0));
        assert_eq!(parse_verdict("Verdict: 4 findings"), Ok(4));
    }

    #[test]
    fn challenge_that_disputes_nothing_is_ignored() {
        let t1 = "## Findings\n(none)\n\n## Challenge\nNone. The brief's premises hold.\n\nVerdict: clean";
        let t2 = "## Challenge\nNone. Premises hold: commit exists on quaqua/sude-e2e1-b and is the HEAD of the review worktree.\n\nVerdict: clean";
        assert!(!disputes(t1) && !disputes(t2));
        assert!(!disputes("## Challenge\n\n## Lessons\nx"));
        assert!(!disputes("## Challenge\n**N/A**\n## Lessons\nx"));
        assert!(!disputes("no section at all"));
    }

    #[test]
    fn real_challenge_disputes() {
        assert!(disputes(
            "## Challenge\nNone of the premises hold: no such commit.\nVerdict: clean"
        ));
        assert!(disputes("## Challenge\nThe SHA is wrong.\n"));
        assert!(disputes(
            "## Challenge\nNone.\n\n## Challenge (again)\nThe branch is missing."
        ));
        assert!(disputes("## Challenge\n- premise 1 is false"));
    }

    #[test]
    fn skeleton_lists_every_outcome_the_engine_parses() {
        let sk = report_skeleton();
        let first = sk.lines().next().unwrap();
        let listed: Vec<&str> = first
            .trim_start_matches(OUTCOME_FIELD)
            .split("   (")
            .next()
            .unwrap()
            .split('|')
            .map(str::trim)
            .collect();
        assert_eq!(listed, OUTCOME_VALUES);
        // each value is a literal the worker judge matches on, so a value
        // dropped from (or added to) the parser fails here
        let judge = include_str!("worker_flow.rs");
        for v in OUTCOME_VALUES {
            assert!(
                judge.contains(&format!("\"{v}\"")),
                "{v} not parsed by worker_flow"
            );
        }
    }
}
