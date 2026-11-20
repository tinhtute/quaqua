//! The CoS situation: what is waiting on the CoS right now, computed from
//! files and `runs/qq.db` (read-only; no network, no new tracking).
//! [`compute`] feeds [`render_block`], the `## Now (computed)` tail of the
//! standing block, which is frozen for the session.
//!
//! Idle time is not recorded anywhere the engine owns: herdr reports a status
//! and a change counter, not a clock, and `agent` keeps only start/end times.
//! So a live agent shows its role and how long it has been up.

use std::path::Path;

use rusqlite::{Connection, OpenFlags};

use super::{Target, TARGETS};

/// Size of the `## Now (computed)` section, in Unicode chars, header included.
pub const NOW_CAP: usize = 600;
/// A capped file at or above this percent of its cap is flagged in the block.
const FILL_WARN: usize = 80;
/// The block's section header.
pub const NOW_HEADER: &str = "## Now (computed)";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Agent {
    pub name: String,
    pub role: String,
    pub up_sec: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fill {
    pub rel: &'static str,
    pub used: usize,
    pub cap: usize,
}

impl Fill {
    pub fn pct(&self) -> usize {
        self.used * 100 / self.cap
    }
    /// `[NN% · used/cap chars]`, chars not bytes.
    pub fn header(&self) -> String {
        format!("[{}% · {}/{} chars]", self.pct(), self.used, self.cap)
    }
}

/// The fill of `text` against the cap of `rel`, if `rel` is capped.
pub fn fill_of(rel: &str, text: &str) -> Option<Fill> {
    TARGETS
        .iter()
        .find(|t| t.rel == rel)
        .map(|&Target { rel, cap }| Fill {
            rel,
            used: text.chars().count(),
            cap,
        })
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Situation {
    /// `reports/audit-*.md` basenames with no `- [audit] <file>:` memory line,
    /// oldest first.
    pub audits: Vec<String>,
    /// Today's memory file: `(workspace-relative path, line count)`; `None`
    /// when it does not exist (or the date is unknown).
    pub today: Option<(String, Option<usize>)>,
    /// Mail for `cos` past its read cursor.
    pub unread: usize,
    pub agents: Vec<Agent>,
    /// Entries of `.quaqua/pending/` (a directory ends with `/`).
    pub pending: Vec<String>,
    pub fills: Vec<Fill>,
}

fn names_in(dir: &Path) -> Vec<(String, bool)> {
    let mut v: Vec<(String, bool)> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().into_string().ok()?;
            Some((name, e.path().is_dir()))
        })
        .collect();
    v.sort();
    v
}

fn unhandled_audits(root: &Path) -> Vec<String> {
    let handled: Vec<String> = names_in(&root.join("memory"))
        .into_iter()
        .filter(|(n, d)| !d && n.ends_with(".md"))
        .filter_map(|(n, _)| std::fs::read_to_string(root.join("memory").join(n)).ok())
        .flat_map(|t| {
            t.lines()
                .filter(|l| l.trim_start().starts_with("- [audit]"))
                .map(String::from)
                .collect::<Vec<_>>()
        })
        .collect();
    names_in(&root.join("reports"))
        .into_iter()
        .filter(|(n, d)| !d && n.starts_with("audit-") && n.ends_with(".md"))
        .map(|(n, _)| n)
        .filter(|n| {
            let key = format!("{n}:");
            !handled.iter().any(|l| l.contains(&key))
        })
        .collect()
}

fn live_agents(root: &Path, now_ms: u64) -> Vec<Agent> {
    let path = root.join("runs/qq.db");
    let Ok(conn) = Connection::open_with_flags(&path, OpenFlags::SQLITE_OPEN_READ_ONLY) else {
        return Vec::new();
    };
    let Ok(mut stmt) = conn.prepare(
        "SELECT name, role, started_ms FROM agent \
         WHERE ended_ms IS NULL AND role <> 'cos' ORDER BY id",
    ) else {
        return Vec::new();
    };
    let rows = stmt.query_map([], |r| {
        let started: i64 = r.get(2)?;
        Ok(Agent {
            name: r.get(0)?,
            role: r.get(1)?,
            up_sec: now_ms.saturating_sub(started.max(0) as u64) / 1000,
        })
    });
    rows.map(|it| it.flatten().collect()).unwrap_or_default()
}

/// Unread mail for `cos`: rows past its cursor, as the bus counts them.
fn unread_mail(root: &Path) -> usize {
    let Ok(conn) =
        Connection::open_with_flags(root.join("runs/qq.db"), OpenFlags::SQLITE_OPEN_READ_ONLY)
    else {
        return 0;
    };
    conn.query_row(
        "SELECT COUNT(*) FROM mail WHERE recipient = 'cos' AND id > \
         COALESCE((SELECT mail_id FROM mail_cursor WHERE recipient = 'cos'), 0)",
        [],
        |r| r.get::<_, i64>(0),
    )
    .map_or(0, |n| n.max(0) as usize)
}

/// The situation of the workspace at `root` at `now_ms` (unix ms); `today` is
/// the local `YYYY-MM-DD` (empty: unknown). Missing files or database read as
/// empty; nothing is written.
pub fn compute(root: &Path, now_ms: u64, today: &str) -> Situation {
    let day = (!today.is_empty()).then(|| format!("memory/{today}.md"));
    Situation {
        audits: unhandled_audits(root),
        today: day.map(|rel| {
            let lines = std::fs::read_to_string(root.join(&rel))
                .ok()
                .map(|t| t.lines().count());
            (rel, lines)
        }),
        unread: unread_mail(root),
        agents: live_agents(root, now_ms),
        pending: names_in(&root.join(".quaqua/pending"))
            .into_iter()
            .map(|(n, d)| if d { format!("{n}/") } else { n })
            .collect(),
        fills: TARGETS
            .iter()
            .filter_map(|t| {
                let text = std::fs::read_to_string(root.join(t.rel)).ok()?;
                fill_of(t.rel, &text)
            })
            .collect(),
    }
}

fn dur(sec: u64) -> String {
    match sec {
        s if s >= 86_400 => format!("{}d", s / 86_400),
        s if s >= 3_600 => format!("{}h", s / 3_600),
        s => format!("{}m", s / 60),
    }
}

/// The newest unhandled audit by name; older ones only as a count.
fn audit_line(audits: &[String]) -> Option<String> {
    let (newest, older) = audits.split_last()?;
    let more = if older.is_empty() {
        String::new()
    } else {
        format!(" (+{} older unhandled)", older.len())
    };
    Some(format!(
        "audit {newest} unhandled{more} → read it, act, log `- [audit] {newest}: <outcome>` in today's memory"
    ))
}

fn today_line((rel, lines): &(String, Option<usize>)) -> String {
    match lines {
        Some(n) => format!("{rel} {n} lines → append `- [slug] <worker> <outcome>` per outcome"),
        None => format!("{rel} missing → create it with the first outcome line"),
    }
}

fn agent_line(a: &Agent) -> String {
    format!(
        "{} {} up {} → `quaqua status`; prompt or clean up if stalled",
        a.role,
        a.name,
        dur(a.up_sec)
    )
}

fn pending_line(p: &str) -> String {
    format!(".quaqua/pending/{p} → diff against the live file, keep one, delete the pending copy")
}

fn fill_line(f: &Fill) -> String {
    format!("{} {} → consolidate with memory_batch", f.rel, f.header())
}

/// `## Now (computed)`: only what needs the CoS, at most [`NOW_CAP`] chars;
/// lines that do not fit become `+N more not shown`.
pub fn render_block(s: &Situation) -> String {
    let mut lines: Vec<String> = audit_line(&s.audits).into_iter().collect();
    if s.unread > 0 {
        lines.push(format!("inbox {} unread → `quaqua inbox`", s.unread));
    }
    lines.extend(s.today.as_ref().map(today_line));
    lines.extend(s.pending.iter().map(|p| pending_line(p)));
    lines.extend(
        s.fills
            .iter()
            .filter(|f| f.pct() >= FILL_WARN)
            .map(fill_line),
    );
    lines.extend(s.agents.iter().map(agent_line));
    if lines.is_empty() {
        lines.push("nothing waiting on you".into());
    }
    let size = |t: &str| t.chars().count();
    let more = |n: usize| format!("+{n} more not shown\n");
    let mut out = format!("{NOW_HEADER}\n");
    let total = lines.len();
    for (i, l) in lines.iter().enumerate() {
        let left = total - i - 1;
        let tail = if left == 0 { 0 } else { size(&more(left)) };
        let line = format!("- {l}\n");
        if size(&out) + size(&line) + tail > NOW_CAP {
            let m = more(total - i);
            if size(&out) + size(&m) <= NOW_CAP {
                out.push_str(&m);
            } else {
                let keep: String = out.chars().take(NOW_CAP - size(&m)).collect();
                out = format!("{keep}{m}");
            }
            return out;
        }
        out.push_str(&line);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    use crate::testkit::TestWorkspace;

    fn ws(tag: &str, files: &[(&str, &str)]) -> TestWorkspace {
        let d = TestWorkspace::new(tag);
        for (p, c) in files {
            let f = d.root().join(p);
            fs::create_dir_all(f.parent().unwrap()).unwrap();
            fs::write(f, c).unwrap();
        }
        d
    }

    fn db(root: &Path, sql: &str) {
        fs::create_dir_all(root.join("runs")).unwrap();
        Connection::open(root.join("runs/qq.db"))
            .unwrap()
            .execute_batch(sql)
            .unwrap();
    }

    #[test]
    fn now_block_names_only_the_newest_unhandled_audit() {
        let d = ws(
            "nb1",
            &[
                ("reports/audit-a.md", "x"),
                ("reports/audit-b.md", "x"),
                ("reports/audit-c.md", "x"),
                ("reports/audit-d.md", "x"),
                ("reports/other.md", "x"),
                (
                    "memory/2026-01-01.md",
                    "- [audit] audit-a.md: done\n- [audit] reports/audit-d.md: done\n- [case] audit-c.md: not an audit line\n",
                ),
            ],
        );
        let s = compute(d.root(), 0, "");
        assert_eq!(
            s.audits,
            vec!["audit-b.md".to_string(), "audit-c.md".to_string()]
        );
        let b = render_block(&s);
        assert!(
            b.contains("audit audit-c.md unhandled (+1 older unhandled) → "),
            "{b}"
        );
        assert!(
            !b.contains("audit-b.md"),
            "older audits are a count only: {b}"
        );
    }

    #[test]
    fn now_block_shows_todays_memory_file_as_path_and_line_count() {
        let d = ws("nb2", &[("memory/2026-11-19.md", "- a\n- b\n- c\n")]);
        let b = render_block(&compute(d.root(), 0, "2026-11-19"));
        assert!(b.contains("memory/2026-11-19.md 3 lines → "), "{b}");
        let b = render_block(&compute(d.root(), 0, "2026-11-20"));
        assert!(
            b.contains("memory/2026-11-20.md missing → create it"),
            "{b}"
        );
        let b = render_block(&compute(d.root(), 0, ""));
        assert!(!b.contains("memory/"), "an unknown date adds no line: {b}");
    }

    #[test]
    fn now_block_counts_unread_mail_past_the_cos_cursor() {
        let d = ws("nb3", &[]);
        db(
            d.root(),
            "CREATE TABLE mail (id INTEGER PRIMARY KEY, recipient TEXT);
             CREATE TABLE mail_cursor (recipient TEXT PRIMARY KEY, mail_id INTEGER);
             INSERT INTO mail VALUES (1,'cos'),(2,'cos'),(3,'cos'),(4,'other');
             INSERT INTO mail_cursor VALUES ('cos',1);",
        );
        let s = compute(d.root(), 0, "");
        assert_eq!(s.unread, 2);
        assert!(render_block(&s).contains("inbox 2 unread → `quaqua inbox`"));
        let none = d.root().join("elsewhere");
        assert_eq!(compute(&none, 0, "").unread, 0, "no database reads as 0");
    }

    #[test]
    fn now_block_lists_pending_files_and_dirs() {
        let d = ws(
            "nb4",
            &[
                (".quaqua/pending/soul.md", "x"),
                (".quaqua/pending/bk/a", "x"),
            ],
        );
        let s = compute(d.root(), 0, "");
        assert_eq!(s.pending, vec!["bk/".to_string(), "soul.md".to_string()]);
        assert!(render_block(&s).contains(".quaqua/pending/soul.md → "));
    }

    #[test]
    fn now_block_lists_live_agents_with_role_and_uptime_cos_excluded() {
        let d = ws("nb5", &[]);
        db(
            d.root(),
            "CREATE TABLE agent (id INTEGER PRIMARY KEY, name TEXT, role TEXT, started_ms INTEGER, ended_ms INTEGER, pane_id TEXT);
             INSERT INTO agent VALUES (1,'cos','cos',0,NULL,'p0'),(2,'a-lead','lead',0,NULL,'p1'),(3,'gone','peer',0,5,'p2');",
        );
        let s = compute(d.root(), 7 * 3_600_000, "");
        assert_eq!(s.agents.len(), 1);
        assert_eq!(
            (s.agents[0].name.as_str(), s.agents[0].up_sec),
            ("a-lead", 25_200)
        );
        assert!(render_block(&s).contains("lead a-lead up 7h → "));
    }

    #[test]
    fn now_block_is_capped_and_says_how_many_it_dropped() {
        let s = Situation {
            pending: (0..30).map(|i| format!("file-{i:02}.md")).collect(),
            ..Default::default()
        };
        let b = render_block(&s);
        assert!(b.chars().count() <= NOW_CAP, "{}", b.chars().count());
        assert!(b.starts_with(NOW_HEADER));
        assert!(b.contains("more not shown"), "{b}");
        assert!(render_block(&Situation::default()).contains("nothing waiting"));
    }

    #[test]
    fn fill_header_counts_chars_not_bytes() {
        let text = "ế".repeat(1100); // 3 bytes each, 1100 chars
        let f = fill_of("USER.md", &text).unwrap();
        assert_eq!((f.used, f.cap, f.pct()), (1100, 1375, 80));
        assert_eq!(f.header(), "[80% · 1100/1375 chars]");
        assert!(fill_of("soul.md", "x").is_none());
    }

    #[test]
    fn fill_header_in_now_only_at_or_above_the_warning() {
        let lo = Situation {
            fills: vec![Fill {
                rel: "USER.md",
                used: 10,
                cap: 100,
            }],
            ..Default::default()
        };
        assert!(!render_block(&lo).contains("USER.md"));
        let hi = Situation {
            fills: vec![Fill {
                rel: "USER.md",
                used: 97,
                cap: 100,
            }],
            ..Default::default()
        };
        assert!(render_block(&hi)
            .contains("USER.md [97% · 97/100 chars] → consolidate with memory_batch"));
    }
}
