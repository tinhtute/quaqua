//! The standing block: persona, owner notes, memory rules and the project
//! index, rendered at SessionStart so the CoS starts with them in context
//! (the hook text lands in the system prompt, ahead of AGENTS.md).

use std::path::Path;

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
const INDEX: &str = "projects/INDEX.md";

struct Section {
    title: &'static str,
    path: &'static str,
    body: String,
}

impl Section {
    fn render(&self) -> String {
        format!("## {} — {}\n{}\n", self.title, self.path, self.body.trim())
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
/// Over budget, the INDEX becomes a pointer; the other sections are never cut,
/// and an overrun they cause alone is stated on the last line.
pub fn render_standing(root: &Path) -> String {
    let sections = load(root);
    if sections.is_empty() {
        return String::new();
    }
    let size = |s: &str| s.chars().count();
    let head = "# Standing context (rendered at session start from the files named in each header; these outrank the global persona)\n\n";
    let mut parts: Vec<String> = sections.iter().map(Section::render).collect();
    if size(head) + parts.iter().map(|p| size(p) + 1).sum::<usize>() > BUDGET {
        if let Some(i) = sections.iter().position(|s| s.path == INDEX) {
            parts[i] = format!("## {} — {INDEX}\nread {INDEX}\n", sections[i].title);
        }
    }
    let mut out = format!("{head}{}", parts.join("\n"));
    let n = size(&out);
    if n > BUDGET {
        out.push_str(&format!(
            "\n(standing block {n} chars, over the {BUDGET} budget: shorten identity.md, SOUL.md, USER.md or memory/rules.md)\n"
        ));
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
        let big = "x".repeat(BUDGET - 300);
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
}
