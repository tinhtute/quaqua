//! T56 / R12: role skill sets come from one table and each driver injects only
//! the set of the agent's role.

use qq_core::authority::Role;
use qq_core::config::{Driver, Harness};
use qq_core::driver::Registry;
use qq_core::skills::{inject, set_for, Delivery, SKILLS};
use qq_core::testkit::TestWorkspace;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

const ORCHESTRATION: &[&str] = &[
    ".claude/skills/herdr-orchestration/SKILL.md",
    ".claude/skills/herdr-orchestration/references/operations.md",
    ".claude/skills/herdr-orchestration/references/recovery.md",
    ".claude/skills/herdr-orchestration/references/judgment.md",
    "playbooks/multi-phase-plan.md",
    "playbooks/shipping.md",
    "playbooks/babysit.md",
    "playbooks/session-pickup.md",
    "playbooks/pause-safely.md",
    "playbooks/worktree-cleanup.md",
    "playbooks/eval.md",
    "playbooks/trace-forensics.md",
];
const IMPLEMENTATION: &[&str] = &[
    "playbooks/feature.md",
    "playbooks/bug-fix.md",
    "playbooks/refactoring.md",
    "playbooks/perf-issue.md",
    "playbooks/investigation.md",
    "playbooks/prototype.md",
    "playbooks/hillclimb.md",
    "playbooks/visual-parity.md",
    "playbooks/runtime-forensics.md",
    "playbooks/figure-it-out.md",
];

fn paths(role: Role) -> BTreeSet<&'static str> {
    set_for(role).iter().map(|s| s.path).collect()
}

fn names(list: &[&'static str]) -> BTreeSet<&'static str> {
    list.iter().copied().collect()
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    for e in std::fs::read_dir(dir).unwrap() {
        let p = e.unwrap().path();
        if p.is_dir() {
            walk(&p, out);
        } else {
            out.push(p);
        }
    }
}

fn git_init(dir: &Path) {
    let ok = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["init", "-q"])
        .status()
        .unwrap()
        .success();
    assert!(ok);
}

fn registry() -> Registry {
    Registry::from_config(&[Harness {
        name: "kimi".into(),
        driver: Driver::Generic,
        spec: None,
    }])
}

#[test]
fn every_template_skill_and_playbook_has_a_role_row() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("templates/workspace");
    let mut files = Vec::new();
    walk(&root.join(".claude/skills"), &mut files);
    walk(&root.join("playbooks"), &mut files);
    assert!(!files.is_empty());
    let rows: BTreeSet<&str> = SKILLS.iter().map(|s| s.path).collect();
    for f in files {
        let rel = f.strip_prefix(&root).unwrap().to_str().unwrap().to_string();
        assert!(rows.contains(rel.as_str()), "no role row for {rel}");
    }
    for s in SKILLS {
        assert!(
            root.join(s.path).is_file(),
            "row without a file: {}",
            s.path
        );
    }
}

#[test]
fn role_sets_follow_the_ruling() {
    assert_eq!(paths(Role::Cos), names(ORCHESTRATION));
    assert_eq!(paths(Role::Lead), names(ORCHESTRATION));
    assert_eq!(paths(Role::Peer), names(IMPLEMENTATION));
    assert!(paths(Role::Supervisor).is_empty());
    // no principle is a skill
    assert!(SKILLS.iter().all(|s| !s.path.starts_with("principles/")));
}

#[test]
fn role_skill_sets_injected_per_driver() {
    let reg = registry();
    for harness in ["claude", "pi", "kimi"] {
        let driver = reg.get(harness).unwrap();
        for role in [Role::Cos, Role::Lead, Role::Peer, Role::Supervisor] {
            let ws = TestWorkspace::new("skills");
            let wt = ws.root().join("wt");
            std::fs::create_dir_all(&wt).unwrap();
            git_init(&wt);
            let got = inject(driver.as_ref(), role, &wt).unwrap();
            let want = paths(role);
            let others: Vec<&str> = SKILLS
                .iter()
                .map(|s| s.path)
                .filter(|p| !want.contains(p))
                .collect();
            let label = format!("{harness}/{}", role.as_str());

            if want.is_empty() {
                assert_eq!(got, Delivery::Nothing, "{label}");
                assert!(
                    !wt.join(".claude").exists() && !wt.join(".qq").exists(),
                    "{label}"
                );
                continue;
            }
            // Every channel carries the text of exactly the role's files.
            let (text, shape) = match (&got, harness) {
                (Delivery::Files(f), "claude") => {
                    assert_eq!(f.len(), want.len(), "{label}");
                    let mut all = Vec::new();
                    walk(&wt.join(".claude/skills"), &mut all);
                    assert_eq!(all.len(), want.len(), "{label}: only the role's files");
                    let mut t = String::new();
                    for p in all {
                        t.push_str(&std::fs::read_to_string(p).unwrap());
                    }
                    (t, "files")
                }
                (Delivery::RulesFile(p), "pi") => {
                    assert!(!wt.join(".claude").exists(), "{label}");
                    (std::fs::read_to_string(wt.join(p)).unwrap(), "rules")
                }
                (Delivery::Prompt(t), "kimi") => {
                    assert!(
                        !wt.join(".claude").exists() && !wt.join(".qq").exists(),
                        "{label}"
                    );
                    (t.clone(), "prompt")
                }
                other => panic!("{label}: wrong channel {other:?}"),
            };
            let _ = shape;
            for s in set_for(role) {
                let marker = s
                    .text
                    .lines()
                    .find(|l| {
                        l.len() > 20
                            && !l.starts_with("---")
                            && !l.starts_with("name:")
                            && !l.starts_with("when:")
                            && !l.starts_with("description:")
                    })
                    .unwrap()
                    .trim();
                assert!(text.contains(marker), "{label}: missing {}", s.path);
            }
            for p in others {
                let s = SKILLS.iter().find(|s| s.path == p).unwrap();
                let marker = s
                    .text
                    .lines()
                    .find(|l| {
                        l.len() > 20
                            && !l.starts_with("---")
                            && !l.starts_with("name:")
                            && !l.starts_with("when:")
                            && !l.starts_with("description:")
                    })
                    .unwrap()
                    .trim();
                if set_for(role).iter().any(|k| k.text.contains(marker)) {
                    continue;
                }
                assert!(!text.contains(marker), "{label}: leaked {p}");
            }
            // Peer never sees orchestration; Lead never sees implementation.
            match role {
                Role::Peer => assert!(!text.contains("Herdr orchestration"), "{label}"),
                Role::Lead => assert!(
                    !text.contains("playbook-feature")
                        && !text.contains("<!-- skill: playbooks/feature.md"),
                    "{label}"
                ),
                _ => {}
            }
        }
    }
}

#[test]
fn claude_playbook_becomes_a_described_skill() {
    let ws = TestWorkspace::new("skills-claude");
    let wt = ws.root().join("wt");
    std::fs::create_dir_all(&wt).unwrap();
    git_init(&wt);
    let reg = registry();
    inject(reg.get("claude").unwrap().as_ref(), Role::Peer, &wt).unwrap();
    let text =
        std::fs::read_to_string(wt.join(".claude/skills/playbook-feature/SKILL.md")).unwrap();
    assert!(text.starts_with("---\nname: playbook-feature\ndescription: New or changed behavior"));
    let status = std::process::Command::new("git")
        .arg("-C")
        .arg(&wt)
        .args(["status", "--porcelain"])
        .output()
        .unwrap();
    assert!(
        status.stdout.is_empty(),
        "injected files must stay untracked-ignored"
    );
}
