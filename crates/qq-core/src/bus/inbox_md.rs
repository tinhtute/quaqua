//! The markdown copy of a Lead -> CoS mail: `runs/<lead>-lead.inbox.md`.
//!
//! The database row is the record; the file is what the CoS reads. One entry
//! per mail id, so writing it again finds nothing left to do.

use std::io::Write;
use std::path::Path;

/// Appends mail `id` to the sender's inbox file when a Lead mails the CoS.
/// Any other mail has no markdown copy. A no-op when the entry is present.
pub fn mirror(
    runs_dir: &Path,
    id: i64,
    from: &str,
    to: &str,
    kind: &str,
    body: &str,
    now_ms: u64,
) -> std::io::Result<()> {
    if to != "cos" || !from.ends_with("-lead") {
        return Ok(());
    }
    let path = runs_dir.join(format!("{from}.inbox.md"));
    let marker = format!("<!-- msg_key: mail:{id} -->");
    let existing = std::fs::read_to_string(&path).unwrap_or_default();
    if existing.lines().any(|l| l.trim() == marker) {
        return Ok(());
    }
    let mut entry = String::new();
    if !existing.is_empty() && !existing.ends_with('\n') {
        entry.push('\n');
    }
    entry.push_str(&format!(
        "## {} {from} {kind}\n{marker}\n<!-- to: {to} -->\n{}\n",
        iso(now_ms),
        escape(body)
    ));
    // One write, append only: a reader never sees half an entry interleaved.
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    f.write_all(entry.as_bytes())
}

/// A body line that would read as a header or a marker gets a leading `\`.
fn escape(body: &str) -> String {
    body.lines()
        .map(|l| {
            let rest = l.trim_start_matches('\\');
            if rest.starts_with("## ") || rest.starts_with("<!--") {
                format!("\\{l}")
            } else {
                l.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// `2026-11-16T10:00:00Z` from epoch milliseconds (civil-from-days).
fn iso(ms: u64) -> String {
    let secs = ms / 1000;
    let (days, rem) = ((secs / 86_400) as i64, secs % 86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}
