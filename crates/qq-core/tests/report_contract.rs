//! The report contract: what a builder or reviewer report must carry to be
//! machine-read. Fixtures copy only the tail SHAPE of five reports that the
//! old binary's watchdog rejected as malformed (exit 6).

use qq_core::task::prompt::report_well_formed;

const SHA: &str = "0123abc4567def890123abc4567def890123abcd";

// 1. builder: Playbook, Blockers, Lessons last (isorev)
#[test]
fn builder_with_playbook_then_lessons_is_accepted() {
    let r = format!("Outcome: COMPLETE\nVerified: unit-test-verified {SHA}\n\n## Playbook\n1: done\n\n## Blockers\nnone\n\n## Lessons\nnone\n");
    assert!(report_well_formed(&r));
}

// 2-3. reviewer: Verdict is the last block, after Blockers (g1rev, g3rev)
#[test]
fn reviewer_verdict_after_blockers_is_accepted() {
    assert!(report_well_formed("Outcome: COMPLETE\n\n## Findings\n- [major] x\n\n## Blockers\nnone\n\nVerdict: 6 findings\n"));
}

// 4. reviewer: no Blockers/Lessons at all, Verdict last (g4rev)
#[test]
fn reviewer_with_only_findings_and_verdict_is_accepted() {
    assert!(report_well_formed("Outcome: COMPLETE\n\n## Findings\n- [minor] y\n\nVerdict: 2 findings\n"));
}

// 5. builder: Blockers then Lessons, no Verified/Playbook (t48anchor)
#[test]
fn builder_without_playbook_is_accepted() {
    assert!(report_well_formed("Outcome: COMPLETE\n\n## Blockers\nnone\n\n## Lessons\nnone\n"));
}

// Drift: the BUILD brief asks only for Outcome + a Verified line, so a report
// that does exactly that must be machine-readable, not "malformed".
#[test]
fn builder_report_as_the_brief_asks_is_accepted() {
    assert!(report_well_formed(&format!("Outcome: COMPLETE\nVerified: unit-test-verified {SHA}\n")));
}

// Drift: parse_verdict takes an indented / lower-case verdict line, so the
// validator must not call the same report malformed.
#[test]
fn verdict_line_shape_parse_verdict_reads_is_accepted() {
    assert!(report_well_formed("## Findings\nnone\n  verdict: clean\n"));
}

// Still unusable: nothing the engine reads.
#[test]
fn prose_only_report_is_malformed() {
    assert!(!report_well_formed("I looked at it and it seems fine.\n"));
}
