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

/// Stable 8-hex id of a finding line (FNV-1a). Digits are stripped first so a
/// count that moves ("3 unpromoted lessons" -> "4") keeps the same id, ack and clock.
pub fn finding_id(line: &str) -> String {
    let mut h: u32 = 0x811c_9dc5;
    for b in line.bytes().filter(|b| !b.is_ascii_digit()) {
        h = (h ^ u32::from(b)).wrapping_mul(0x0100_0193);
    }
    format!("{h:08x}")
}

/// Live-state findings are never hidden; only report-only ones may be acked or expire.
pub fn is_report_only(line: &str) -> bool {
    !(line.contains("herdr")
        || line.contains("live worker")
        || line.contains("lost momentum")
        || line.starts_with("- mail #"))
}

fn parse(store: &str) -> Vec<(String, u64, bool)> {
    store
        .lines()
        .filter_map(|l| {
            let mut p = l.split('\t');
            let id = p.next()?.to_string();
            let seen = p.next()?.parse().ok()?;
            Some((id, seen, p.next() == Some("ack")))
        })
        .collect()
}

fn render(rows: &[(String, u64, bool)]) -> String {
    rows.iter()
        .map(|(id, seen, a)| format!("{id}\t{seen}\t{}\n", if *a { "ack" } else { "seen" }))
        .collect()
}

/// Returns the findings still to print and the updated store text. A report-only
/// finding is recorded on first sight; it is dropped once acked or older than
/// `EXPIRY_DAYS`. Store rows for findings no longer raised are forgotten, so a
/// recurrence after the problem was fixed starts a fresh clock.
pub fn apply(flags: Vec<String>, store: &str, now_secs: u64) -> (Vec<String>, String) {
    let old = parse(store);
    let mut rows: Vec<(String, u64, bool)> = Vec::new();
    let mut kept = Vec::new();
    for f in flags {
        if !is_report_only(&f) {
            kept.push(f);
            continue;
        }
        let id = finding_id(&f);
        let (seen, acked) = old
            .iter()
            .find(|r| r.0 == id)
            .map(|r| (r.1, r.2))
            .unwrap_or((now_secs, false));
        if !rows.iter().any(|r| r.0 == id) {
            rows.push((id, seen, acked));
        }
        if !acked && now_secs.saturating_sub(seen) <= EXPIRY_DAYS * 86_400 {
            kept.push(f);
        }
    }
    (kept, render(&rows))
}

/// Marks `id` acked in `store`; `None` when no such finding is recorded.
pub fn ack(store: &str, id: &str, _now_secs: u64) -> Option<String> {
    let mut rows = parse(store);
    let row = rows.iter_mut().find(|r| r.0 == id)?;
    row.2 = true;
    Some(render(&rows))
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
    fn auditor_ack_id_ignores_counts() {
        let a = "- runs/: 3 unpromoted lessons (> 7d)";
        let b = "- runs/: 12 unpromoted lessons (> 7d)";
        assert_eq!(finding_id(a), finding_id(b));
        let (_, store) = apply(vec![a.into()], "", 0);
        let store = ack(&store, &finding_id(a), 1).unwrap();
        let (kept, store) = apply(vec![b.into()], &store, 5 * DAY);
        assert!(kept.is_empty(), "count change must not lose the ack");
        assert!(store.contains("\t0\tack"), "first-seen clock kept: {store}");
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
