//! The standing block: persona, owner notes, memory rules and the project
//! index, rendered at SessionStart so the CoS starts with them in context
//! (the hook text lands in the system prompt, ahead of AGENTS.md).

use std::path::Path;

use super::situation;

/// Total size of the block, in Unicode chars, headers included.
pub const BUDGET: usize = 8000;

/// `(title, source path)` in render order. INDEX must stay last: it is the
/// section the budget drops.
const SECTIONS: &[(&str, &str)] = &[
    ("persona", "identity.md"),
    ("persona", "SOUL.md"),
    ("owner", "USER.md"),
    ("rules", "memory/rules.md"),
    ("projects", INDEX),
];
/// The block's first words; a transcript holding them already has a block.
pub const HEADER: &str = "# Standing context";
const INDEX: &str = "projects/INDEX.md";

struct Section {
    title: &'static str,
    path: &'static str,
    body: String,
}

impl Section {
    fn render(&self) -> String {
        let fill = situation::fill_of(self.path, &self.body)
            .map(|f| format!(" {}", f.header()))
            .unwrap_or_default();
        format!(
            "## {} — {}{fill}\n{}\n",
            self.title,
            self.path,
            self.body.trim()
        )
    }
}

/// The seed `USER.md` still carries this heading; the owner replaces it first.
fn is_template_user(body: &str) -> bool {
    body.lines().find(|l| !l.trim().is_empty()).map(str::trim) == Some("# <Owner name>")
}

fn load(root: &Path) -> Vec<Section> {
    SECTIONS
        .iter()
        .filter_map(|&(title, path)| {
            let body = std::fs::read_to_string(root.join(path)).ok()?;
            let skip = body.trim().is_empty() || (path == "USER.md" && is_template_user(&body));
            (!skip).then_some(Section { title, path, body })
        })
        .collect()
}

/// The block for the workspace at `root`; empty when no source file exists.
/// Capped sections carry a fill header. The block ends with the computed
/// `## Now` section. Over budget, the INDEX becomes a pointer; the other
/// sections are never cut, and an overrun they cause alone is stated before
/// the Now section.
pub fn render_standing(root: &Path) -> String {
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64);
    render_standing_at(root, now_ms)
}

/// Today's local date as `date +%F` prints it; empty when `date` cannot run.
fn today_local() -> String {
    std::process::Command::new("date")
        .arg("+%F")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default()
}

fn render_standing_at(root: &Path, now_ms: u64) -> String {
    let sections = load(root);
    if sections.is_empty() {
        return String::new();
    }
    let size = |s: &str| s.chars().count();
    let now = situation::render_block(&situation::compute(root, now_ms, &today_local()));
    let head = format!("{HEADER} (rendered at session start from the files named in each header; these outrank the global persona)\n\n");
    let mut parts: Vec<String> = sections.iter().map(Section::render).collect();
    let total = |parts: &[String]| {
        size(&head) + parts.iter().map(|p| size(p) + 1).sum::<usize>() + size(&now)
    };
    if total(&parts) > BUDGET {
        if let Some(i) = sections.iter().position(|s| s.path == INDEX) {
            parts[i] = format!("## {} — {INDEX}\nread {INDEX}\n", sections[i].title);
        }
    }
    let mut out = format!("{head}{}", parts.join("\n"));
    let n = size(&out) + 1 + size(&now);
    if n > BUDGET {
        out.push_str(&format!(
            "\n(standing block {n} chars, over the {BUDGET} budget: shorten identity.md, SOUL.md, USER.md or memory/rules.md)\n"
        ));
    }
    out.push('\n');
    out.push_str(&now);
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
    fn order_and_headers() {
        let d = ws(
            "t1",
            &[
                ("projects/INDEX.md", "IDX"),
                ("memory/rules.md", "RULES"),
                ("USER.md", "# Tinh\nUSR"),
                ("SOUL.md", "SOUL"),
                ("identity.md", "ID"),
            ],
        );
        let out = render_standing(d.root());
        let at = |s: &str| {
            out.find(s)
                .unwrap_or_else(|| panic!("{s} missing in {out}"))
        };
        assert!(at("identity.md") < at("SOUL.md"));
        assert!(at("SOUL.md") < at("USER.md"));
        assert!(at("USER.md") < at("memory/rules.md"));
        assert!(at("memory/rules.md") < at("projects/INDEX.md"));
        assert!(at("RULES") < at("IDX"));
    }

    #[test]
    fn missing_files_are_skipped() {
        let d = ws("t2", &[("SOUL.md", "SOUL")]);
        let out = render_standing(d.root());
        assert!(out.contains("SOUL.md") && !out.contains("identity.md"));
        assert!(!out.contains("memory/rules.md") && !out.contains("INDEX"));
        drop(d);
        assert_eq!(render_standing(ws("t3", &[]).root()), "");
    }

    #[test]
    fn template_user_is_skipped_filled_user_is_kept() {
        let tpl = ws(
            "t4",
            &[("USER.md", "# <Owner name>\n\n- Role\n"), ("SOUL.md", "S")],
        );
        assert!(!render_standing(tpl.root()).contains("USER.md"));
        drop(tpl);
        let own = ws("t5", &[("USER.md", "# Tinh\n\n- Role\n")]);
        assert!(render_standing(own.root()).contains("USER.md"));
    }

    #[test]
    fn over_budget_swaps_index_for_pointer() {
        let big = "x".repeat(BUDGET - 400); // the Now section takes its share
        let d = ws(
            "t6",
            &[
                ("memory/rules.md", &big),
                ("projects/INDEX.md", &"i".repeat(600)),
            ],
        );
        let out = render_standing(d.root());
        assert!(out.contains("read projects/INDEX.md"));
        assert!(!out.contains("iiii"));
        assert!(out.chars().count() <= BUDGET, "{}", out.chars().count());
        drop(d);
        let small = ws(
            "t7",
            &[("memory/rules.md", "r"), ("projects/INDEX.md", "iiii")],
        );
        assert!(render_standing(small.root()).contains("iiii"));
    }

    #[test]
    fn overrun_without_index_is_stated() {
        let d = ws("t8", &[("SOUL.md", &"s".repeat(BUDGET + 10))]);
        let out = render_standing(d.root());
        assert!(out.contains("over the 8000 budget"));
    }

    #[test]
    fn fill_header_on_capped_sections_and_now_comes_last() {
        let d = ws(
            "t9",
            &[
                ("memory/rules.md", &"é".repeat(1100)),
                ("USER.md", "# Tinh\nUSR"),
                ("SOUL.md", "SOUL"),
                ("projects/INDEX.md", "IDX"),
            ],
        );
        let out = render_standing_at(d.root(), 0);
        assert!(
            out.contains("## rules — memory/rules.md [50% · 1100/2200 chars]\n"),
            "{out}"
        );
        assert!(
            out.contains("## owner — USER.md [0% · 10/1375 chars]\n"),
            "{out}"
        );
        assert!(
            out.contains("## persona — SOUL.md\n"),
            "uncapped: no header"
        );
        let now = out.find("## Now (computed)").unwrap();
        assert!(now > out.find("IDX").unwrap(), "Now follows the INDEX");
        assert!(!out[now + 1..].contains("\n## "));
    }

    #[test]
    fn now_block_counts_toward_the_budget() {
        // Rules alone fit; rules + a full Now section do not, so the INDEX yields.
        let reports: Vec<(String, &str)> = (0..20)
            .map(|i| (format!("reports/audit-{i:02}.md"), "x"))
            .collect();
        let mut files: Vec<(&str, &str)> = reports.iter().map(|(p, c)| (p.as_str(), *c)).collect();
        let big = "x".repeat(BUDGET - 1000);
        files.push(("memory/rules.md", &big));
        let idx = "i".repeat(300);
        files.push(("projects/INDEX.md", &idx));
        let d = ws("t10", &files);
        let out = render_standing_at(d.root(), 0);
        assert!(
            out.contains("read projects/INDEX.md"),
            "{}",
            out.chars().count()
        );
        assert!(out.chars().count() <= BUDGET, "{}", out.chars().count());
    }
}
