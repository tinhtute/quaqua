//! The CoS situation: what is waiting on the CoS right now, computed from
//! files and the agent table (no network, no new tracking). One `compute`
//! feeds two views: [`render_cli`] (`quaqua memory situation`) and
//! [`render_block`] (the `## Now (computed)` tail of the standing block).
//!
//! Idle time is not recorded anywhere the engine owns: herdr reports a status
//! and a change counter, not a clock, and `agent` keeps only start/end times.
//! So an agent shows its role, how long it has been up, and (CLI only) the
//! herdr status the caller fills in with [`Situation::set_status`].

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
    pub pane_id: Option<String>,
    /// herdr's `agent_status`; filled by the CLI only.
    pub status: Option<String>,
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
    /// `reports/audit-*.md` basenames with no `- [audit] <file>:` memory line.
    pub audits: Vec<String>,
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
        "SELECT name, role, started_ms, pane_id FROM agent \
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
            pane_id: r.get(3)?,
            status: None,
        })
    });
    rows.map(|it| it.flatten().collect()).unwrap_or_default()
}

/// The situation of the workspace at `root` at `now_ms` (unix ms). Missing
/// files or database read as empty; nothing is written.
pub fn compute(root: &Path, now_ms: u64) -> Situation {
    Situation {
        audits: unhandled_audits(root),
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

impl Situation {
    /// Adds herdr statuses, looked up by pane id.
    pub fn set_status(&mut self, status_of: impl Fn(&str) -> Option<String>) {
        for a in &mut self.agents {
            a.status = a.pane_id.as_deref().and_then(&status_of);
        }
    }
}

fn dur(sec: u64) -> String {
    match sec {
        s if s >= 86_400 => format!("{}d", s / 86_400),
        s if s >= 3_600 => format!("{}h", s / 3_600),
        s => format!("{}m", s / 60),
    }
}

fn audit_line(f: &str) -> String {
    format!("audit {f} unhandled → read it, act, log `- [audit] {f}: <outcome>` in today's memory")
}

fn agent_line(a: &Agent) -> String {
    let st = a
        .status
        .as_deref()
        .map(|s| format!(", {s}"))
        .unwrap_or_default();
    format!(
        "{} {} up {}{st} → `quaqua status`; prompt or clean up if stalled",
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

/// The CLI view: every item, and every capped file's fill. One action per line.
pub fn render_cli(s: &Situation) -> String {
    let mut out: Vec<String> = s.audits.iter().map(|f| audit_line(f)).collect();
    out.extend(s.agents.iter().map(agent_line));
    out.extend(s.pending.iter().map(|p| pending_line(p)));
    out.extend(s.fills.iter().map(|f| {
        if f.pct() >= FILL_WARN {
            fill_line(f)
        } else {
            format!("{} {} → ok", f.rel, f.header())
        }
    }));
    out.join("\n")
}

/// The block view: `## Now (computed)` with only what needs the CoS, at most
/// [`NOW_CAP`] chars; lines that do not fit become `+N more`.
pub fn render_block(s: &Situation) -> String {
    let mut lines: Vec<String> = s.audits.iter().map(|f| audit_line(f)).collect();
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
    let mut out = format!("{NOW_HEADER}\n");
    let total = lines.len();
    for (i, l) in lines.iter().enumerate() {
        let left = total - i - 1;
        let more = format!("+{} more → `quaqua memory situation`\n", left + 1);
        let tail = if left == 0 { 0 } else { size(&more) };
        let line = format!("- {l}\n");
        if size(&out) + size(&line) + tail > NOW_CAP {
            let more = format!("+{} more → `quaqua memory situation`\n", total - i);
            // Even `+N more` must fit: cut the line before it if it does not.
            if size(&out) + size(&more) <= NOW_CAP {
                out.push_str(&more);
            } else {
                let keep: String = out.chars().take(NOW_CAP - size(&more)).collect();
                out = format!("{keep}{more}");
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

    #[test]
    fn audit_is_handled_only_by_its_own_memory_line() {
        let d = ws(
            "sit1",
            &[
                ("reports/audit-a.md", "x"),
                ("reports/audit-b.md", "x"),
                ("reports/audit-c.md", "x"),
                ("reports/other.md", "x"),
                ("memory/2026-01-01.md", "- [audit] audit-a.md: done\n- [audit] reports/audit-b.md: done\n- [case] audit-c.md: not an audit line\n"),
            ],
        );
        let s = compute(d.root(), 0);
        assert_eq!(s.audits, vec!["audit-c.md".to_string()]);
        assert!(render_cli(&s).contains("audit audit-c.md unhandled → "));
    }

    #[test]
    fn pending_files_and_dirs_are_listed() {
        let d = ws(
            "sit2",
            &[
                (".quaqua/pending/models.md", "x"),
                (".quaqua/pending/bk/a", "x"),
            ],
        );
        let s = compute(d.root(), 0);
        assert_eq!(s.pending, vec!["bk/".to_string(), "models.md".to_string()]);
    }

    #[test]
    fn fill_counts_chars_not_bytes() {
        let text = "ế".repeat(1100); // 3 bytes each, 1100 chars
        let f = fill_of("USER.md", &text).unwrap();
        assert_eq!((f.used, f.cap, f.pct()), (1100, 1375, 80));
        assert_eq!(f.header(), "[80% · 1100/1375 chars]");
        assert!(fill_of("SOUL.md", "x").is_none());
    }

    #[test]
    fn live_agents_come_from_the_agent_table_cos_excluded() {
        let d = ws("sit3", &[]);
        fs::create_dir_all(d.root().join("runs")).unwrap();
        let c = Connection::open(d.root().join("runs/qq.db")).unwrap();
        c.execute_batch(
            "CREATE TABLE agent (id INTEGER PRIMARY KEY, name TEXT, role TEXT, started_ms INTEGER, ended_ms INTEGER, pane_id TEXT);
             INSERT INTO agent VALUES (1,'cos','cos',0,NULL,'p0'),(2,'a-lead','lead',0,NULL,'p1'),(3,'gone','peer',0,5,'p2');",
        )
        .unwrap();
        drop(c);
        let mut s = compute(d.root(), 7 * 3_600_000);
        assert_eq!(s.agents.len(), 1);
        assert_eq!(
            (s.agents[0].name.as_str(), s.agents[0].up_sec),
            ("a-lead", 25_200)
        );
        s.set_status(|p| (p == "p1").then(|| "idle".to_string()));
        assert!(render_cli(&s).contains("lead a-lead up 7h, idle → "));
    }

    #[test]
    fn block_is_capped_and_says_how_many_it_dropped() {
        let s = Situation {
            audits: (0..30)
                .map(|i| format!("audit-2026-11-{i:02}.md"))
                .collect(),
            ..Default::default()
        };
        let b = render_block(&s);
        assert!(b.chars().count() <= NOW_CAP, "{}", b.chars().count());
        assert!(b.starts_with(NOW_HEADER));
        assert!(b.contains("more → `quaqua memory situation`"), "{b}");
        let calm = render_block(&Situation::default());
        assert!(calm.contains("nothing waiting"));
    }

    #[test]
    fn block_omits_fill_below_the_warning_and_names_the_action_above() {
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
