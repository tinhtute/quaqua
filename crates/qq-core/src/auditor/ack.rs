//! Acknowledge or expire report-only findings so audits stop repeating them.
//!
//! State lives in `<root>/reports/auditor-acks.tsv`, one `id<TAB>first_seen<TAB>state`
//! line per finding (`state` is `seen` or `ack`). A finding is suppressed once it
//! is acked, or once it was first seen more than `EXPIRY_DAYS` days ago.

use std::path::{Path, PathBuf};

pub const EXPIRY_DAYS: u64 = 14;

pub fn store_path(root: &Path) -> PathBuf {
    root.join("reports").join("auditor-acks.tsv")
}

/// Stable 8-hex id of a finding line (FNV-1a).
pub fn finding_id(line: &str) -> String {
    let _ = line;
    String::new()
}

/// Live-state findings are never hidden; only report-only ones may be acked or expire.
pub fn is_report_only(line: &str) -> bool {
    let _ = line;
    true
}

/// Returns the findings still to print and the updated store text.
pub fn apply(flags: Vec<String>, store: &str, now_secs: u64) -> (Vec<String>, String) {
    let _ = (store, now_secs);
    (flags, String::new())
}

/// Marks `id` acked in `store`; `false` when no such finding was ever recorded.
pub fn ack(store: &str, id: &str, now_secs: u64) -> Option<String> {
    let _ = (store, id, now_secs);
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    const DAY: u64 = 86_400;
    const RO: &str = "- AGENTS.md: 120 lines (>100) — doc-rot risk, trim or split";
    const LIVE: &str = "- herdr: worker 'x' is stalled";

    #[test]
    fn auditor_ack_id_is_stable_and_distinct() {
        assert_eq!(finding_id(RO), finding_id(RO));
        assert_eq!(finding_id(RO).len(), 8);
        assert_ne!(finding_id(RO), finding_id("- other"));
    }

    #[test]
    fn auditor_ack_suppresses_acked_finding() {
        let (kept, store) = apply(vec![RO.into()], "", 1000);
        assert_eq!(kept, vec![RO.to_string()]);
        let store = ack(&store, &finding_id(RO), 1100).expect("known id");
        let (kept, _) = apply(vec![RO.into()], &store, 1200);
        assert!(kept.is_empty());
        assert!(ack(&store, "deadbeef", 1).is_none());
    }

    #[test]
    fn auditor_ack_expires_after_age() {
        let (_, store) = apply(vec![RO.into()], "", 0);
        let (kept, store) = apply(vec![RO.into()], &store, EXPIRY_DAYS * DAY);
        assert_eq!(kept.len(), 1, "exactly at the limit still prints");
        let (kept, _) = apply(vec![RO.into()], &store, EXPIRY_DAYS * DAY + 1);
        assert!(kept.is_empty());
    }

    #[test]
    fn auditor_ack_never_hides_live_state() {
        let (_, store) = apply(vec![LIVE.into()], "", 0);
        let (kept, _) = apply(vec![LIVE.into()], &store, 100 * DAY);
        assert_eq!(kept, vec![LIVE.to_string()]);
    }
}
