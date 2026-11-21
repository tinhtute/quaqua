//! The sensor table (R14): per loop, the guides, the computational sensors
//! (run first) and the inferential ones. A prose rule repeated twice becomes
//! a check: the repeated-rule counter lives in [`crate::supervisor::job`].

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Computational,
    Inferential,
}

pub struct LoopRow {
    pub name: &'static str,
    pub guides: &'static [&'static str],
    pub computational: &'static [&'static str],
    pub inferential: &'static [&'static str],
}

pub const LOOPS: [LoopRow; 4] = [
    LoopRow {
        name: "inner",
        guides: &["brief", "role skill set", "worker rules", "protocol (Lead only)"],
        computational: &[
            "report file",
            "required checks of the protocol",
            "candidate-sha binding",
            "fence and topology refusals",
        ],
        inferential: &["cross-harness review"],
    },
    LoopRow {
        name: "outer 1",
        guides: &["risk split"],
        computational: &[
            "farm test and gate failure source",
            "duplicate check by failure signature",
        ],
        inferential: &["CoS verdict on a low-risk proposal"],
    },
    LoopRow {
        name: "outer 2",
        guides: &["rubrics seeded from the 17 anti-patterns"],
        computational: &[
            "auditor",
            "static-analysis check",
            "architecture check",
            "repeated-rule counter",
        ],
        inferential: &["conversation scoring", "supervisor agent question to the Lead"],
    },
    LoopRow {
        name: "outer 3",
        guides: &["idea inbox"],
        computational: &["inbox parse and duplicate check"],
        inferential: &["CoS triage", "anh iu decision on high risk"],
    },
];

/// The run order of one row: every computational sensor, then every inferential one.
pub fn order(row: &LoopRow) -> Vec<(Kind, &'static str)> {
    row.computational
        .iter()
        .map(|s| (Kind::Computational, *s))
        .chain(row.inferential.iter().map(|s| (Kind::Inferential, *s)))
        .collect()
}
