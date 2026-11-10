//! Tests for `quaqua recall <id>`: trigger → thread → ref, at most two hops.

use quaqua::bus::recall::{extract_commit, load_recall, render_text, RefKind, REF_HEAD_LINES};
use quaqua::bus::{FarmBus, NewTrigger};
use std::path::PathBuf;

struct Fixture {
    root: PathBuf,
    /// A sibling of `root`, outside the workspace, for escape attempts.
    outside: PathBuf,
}

impl Fixture {
    fn new(tag: &str) -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "quaqua-recall-test-{tag}-{}-{nanos}",
            std::process::id()
        ));
        std::fs::create_dir_all(root.join("runs")).expect("create fixture root");
        let outside = root.with_file_name(format!(
            "{}-outside",
            root.file_name().unwrap().to_string_lossy()
        ));
        std::fs::create_dir_all(&outside).expect("create outside dir");
        Self { root, outside }
    }

    fn runs(&self) -> PathBuf {
        self.root.join("runs")
    }

    fn bus(&self) -> FarmBus {
        FarmBus::open(&self.runs().join("quaqua.db")).expect("open bus")
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
        let _ = std::fs::remove_dir_all(&self.outside);
    }
}

fn record(bus: &FarmBus, trigger: NewTrigger) -> u64 {
    bus.record_trigger(&trigger).expect("record trigger");
    bus.conn_for_test()
        .query_row("SELECT MAX(id) FROM triggers", [], |r| r.get::<_, i64>(0))
        .expect("max trigger id") as u64
}

fn trigger(kind: &str, thread: &str, summary: &str) -> NewTrigger {
    NewTrigger::new("mail", kind, "suhuynh", thread, summary)
}

fn canonical(path: &std::path::Path) -> String {
    path.canonicalize()
        .expect("canonicalize fixture path")
        .display()
        .to_string()
}

#[test]
fn recall_prints_trigger_thread_owner_and_five_most_recent_triggers() {
    let fx = Fixture::new("thread");
    let bus = fx.bus();
    bus.open_thread("quaqua/superagent-integ", "superagent-lead")
        .expect("open thread");

    let mut ids = Vec::new();
    for i in 0..6 {
        ids.push(record(
            &bus,
            trigger(
                "handoff",
                "quaqua/superagent-integ",
                &format!("summary {i}"),
            ),
        ));
    }

    let recall = load_recall(&bus, ids[2], &fx.root)
        .expect("load recall")
        .expect("trigger exists");

    assert_eq!(recall.trigger.id, ids[2]);
    assert_eq!(recall.trigger.summary, "summary 2");
    assert_eq!(recall.trigger.payload_json, "{}");
    assert_eq!(recall.thread.id, "quaqua/superagent-integ");
    assert_eq!(recall.thread.owner, "superagent-lead");
    assert_eq!(recall.thread.state, "open");

    let recent_ids: Vec<u64> = recall.thread.recent.iter().map(|t| t.id).collect();
    assert_eq!(
        recent_ids,
        vec![ids[5], ids[4], ids[3], ids[2], ids[1]],
        "the 5 newest triggers, newest first"
    );

    let text = render_text(&recall);
    assert!(text.contains(&format!("trigger #{}", ids[2])));
    assert!(text.contains("owner: superagent-lead"));
    assert!(text.contains("state: open"));
    assert!(text.contains("summary 2"));
    assert!(text.contains(&format!("#{} handoff suhuynh: summary 5", ids[5])));
    assert!(
        !text.contains("summary 0"),
        "the sixth-newest trigger is outside the window"
    );
}

#[test]
fn recall_ref_prints_path_and_first_forty_lines() {
    let fx = Fixture::new("ref-head");
    let bus = fx.bus();
    bus.open_thread("quaqua/ref", "lead").expect("open thread");

    let notes = fx.root.join("docs/notes.txt");
    std::fs::create_dir_all(notes.parent().unwrap()).expect("create docs dir");
    let mut body = String::from("Snapshot: 3a3cd55\n");
    for i in 0..49 {
        body.push_str(&format!("line {i}\n"));
    }
    std::fs::write(&notes, body).expect("write notes");

    let id = record(
        &bus,
        trigger("handoff", "quaqua/ref", "notes").with_ref("docs/notes.txt"),
    );
    let recall = load_recall(&bus, id, &fx.root).unwrap().unwrap();
    let detail = recall.ref_detail.as_ref().expect("ref resolved");

    assert_eq!(detail.kind, RefKind::Path);
    assert_eq!(detail.path, canonical(&notes));
    assert!(detail.exists);
    assert_eq!(detail.head.len(), REF_HEAD_LINES);
    assert_eq!(detail.head.first().unwrap(), "Snapshot: 3a3cd55");
    assert_eq!(detail.head.last().unwrap(), "line 38");
    assert!(detail.truncated);
    assert_eq!(
        detail.commit, None,
        "a commit SHA outside a runs/ report is not extracted"
    );

    let text = render_text(&recall);
    assert!(text.contains(&format!("ref {}", canonical(&notes))));
    assert!(text.contains("Snapshot: 3a3cd55"));
    assert!(text.contains("line 38"));
    assert!(
        !text.contains("line 39"),
        "line 41 is past the 40-line head"
    );
    assert!(text.contains("(truncated)"));
}

#[test]
fn recall_absolute_ref_outside_workspace_is_not_read() {
    let fx = Fixture::new("abs-outside");
    let bus = fx.bus();
    bus.open_thread("quaqua/abs", "lead").expect("open thread");

    let outside = fx.outside.join("abs.txt");
    std::fs::write(&outside, "absolute ref body\n").expect("write outside file");
    let id = record(
        &bus,
        trigger("handoff", "quaqua/abs", "abs").with_ref(outside.to_str().unwrap()),
    );

    let recall = load_recall(&bus, id, &fx.root).unwrap().unwrap();
    let detail = recall.ref_detail.as_ref().unwrap();
    assert_eq!(detail.kind, RefKind::Outside);
    assert!(!detail.exists, "an outside ref is reported, not resolved");
    assert!(detail.head.is_empty(), "an outside ref is never read");
    assert_eq!(detail.commit, None);

    let text = render_text(&recall);
    assert!(text.contains("(outside the workspace"), "{text}");
    assert!(
        !text.contains("absolute ref body"),
        "the file body never leaks: {text}"
    );
}

#[test]
fn recall_parent_escape_ref_is_not_read() {
    let fx = Fixture::new("dotdot");
    let bus = fx.bus();
    bus.open_thread("quaqua/dotdot", "lead").expect("open thread");

    let outside = fx.outside.join("dotdot.txt");
    std::fs::write(&outside, "parent escape body\n").expect("write outside file");
    let ref_val = format!(
        "../{}/dotdot.txt",
        fx.outside.file_name().unwrap().to_string_lossy()
    );
    let id = record(
        &bus,
        trigger("handoff", "quaqua/dotdot", "dotdot").with_ref(&ref_val),
    );

    let recall = load_recall(&bus, id, &fx.root).unwrap().unwrap();
    let detail = recall.ref_detail.as_ref().unwrap();
    assert_eq!(detail.kind, RefKind::Outside);
    assert!(!detail.exists, "`..` must not escape the workspace");
    assert!(detail.head.is_empty());
    assert!(
        !render_text(&recall).contains("parent escape body"),
        "the file body never leaks"
    );
}

#[cfg(unix)]
#[test]
fn recall_symlink_escape_ref_is_not_read() {
    let fx = Fixture::new("symlink");
    let bus = fx.bus();
    bus.open_thread("quaqua/link", "lead").expect("open thread");

    let target = fx.outside.join("target.txt");
    std::fs::write(&target, "symlink target body\n").expect("write target");
    std::os::unix::fs::symlink(&target, fx.root.join("escape.txt")).expect("create symlink");

    let id = record(
        &bus,
        trigger("handoff", "quaqua/link", "link").with_ref("escape.txt"),
    );

    let recall = load_recall(&bus, id, &fx.root).unwrap().unwrap();
    let detail = recall.ref_detail.as_ref().unwrap();
    assert_eq!(detail.kind, RefKind::Outside);
    assert!(!detail.exists, "a symlink out of the workspace is not followed");
    assert!(detail.head.is_empty());
    assert!(!render_text(&recall).contains("symlink target body"));
}

/// A missing component followed by `..` gives the lenient resolver a prefix it
/// cannot canonicalize; the `..` must not be popped lexically, because that hides
/// the symlink the rest of the path goes through.
#[cfg(unix)]
#[test]
fn recall_symlink_escape_through_missing_component_is_not_read() {
    let fx = Fixture::new("missing-dotdot-link");
    let bus = fx.bus();
    bus.open_thread("quaqua/miss", "lead").expect("open thread");

    let secret = fx.outside.join("secret.txt");
    std::fs::write(&secret, "outside secret body\n").expect("write secret");
    std::os::unix::fs::symlink(&fx.outside, fx.root.join("link")).expect("create symlink");

    for raw in ["nope/../link/secret.txt", "runs/../nope/../link/secret.txt"] {
        let id = record(
            &bus,
            trigger("handoff", "quaqua/miss", "misslink").with_ref(raw),
        );
        let recall = load_recall(&bus, id, &fx.root).unwrap().unwrap();
        let detail = recall.ref_detail.as_ref().unwrap();

        assert_eq!(
            detail.kind,
            RefKind::Outside,
            "{raw} cannot be proven inside the workspace"
        );
        assert!(!detail.exists, "{raw} is not a workspace file");
        assert!(detail.head.is_empty(), "{raw} is never read");
        let text = render_text(&recall);
        assert!(
            !text.contains("outside secret body"),
            "{raw} leaked the outside file: {text}"
        );
    }
}

#[test]
fn recall_scheme_refs_are_printed_verbatim() {
    let fx = Fixture::new("scheme");
    let bus = fx.bus();
    bus.open_thread("quaqua/scheme", "lead").expect("open thread");

    for raw in [
        "mail:12",
        "project:quaqua",
        "mail:12;project:quaqua",
        "https://github.com/tinhtute/quaqua/actions/runs/123",
        "http://example.com/runs/1",
    ] {
        let id = record(
            &bus,
            trigger("handoff", "quaqua/scheme", "scheme").with_ref(raw),
        );
        let recall = load_recall(&bus, id, &fx.root).unwrap().unwrap();
        let detail = recall.ref_detail.as_ref().unwrap();
        assert_eq!(detail.kind, RefKind::Scheme);
        assert_eq!(detail.path, raw, "a scheme ref is printed as written");
        assert!(!detail.exists, "{raw} is not a workspace file");
        assert!(detail.head.is_empty(), "{raw} is never read");
        assert_eq!(detail.commit, None);

        let text = render_text(&recall);
        assert!(
            text.contains(&format!("\nref {raw}\n")),
            "a scheme ref is printed as written: {text}"
        );
        assert!(
            !text.contains("(missing)"),
            "{raw} is not a missing workspace file: {text}"
        );
    }
}

#[test]
fn recall_path_ref_with_colon_is_not_a_scheme() {
    let fx = Fixture::new("colon");
    let bus = fx.bus();
    bus.open_thread("quaqua/colon", "lead").expect("open thread");

    let notes = fx.root.join("docs/notes:1.md");
    std::fs::create_dir_all(notes.parent().unwrap()).expect("create docs dir");
    std::fs::write(&notes, "colon path body\n").expect("write notes");
    let id = record(
        &bus,
        trigger("handoff", "quaqua/colon", "colon").with_ref("docs/notes:1.md"),
    );

    let recall = load_recall(&bus, id, &fx.root).unwrap().unwrap();
    let detail = recall.ref_detail.as_ref().unwrap();
    assert_eq!(detail.kind, RefKind::Path);
    assert!(detail.exists);
    assert_eq!(detail.head, vec!["colon path body".to_string()]);
}

#[test]
fn recall_parent_ref_inside_workspace_is_read() {
    let fx = Fixture::new("inside-dotdot");
    let bus = fx.bus();
    bus.open_thread("quaqua/inside", "lead").expect("open thread");

    let notes = fx.root.join("docs/notes.txt");
    std::fs::create_dir_all(notes.parent().unwrap()).expect("create docs dir");
    std::fs::write(&notes, "inside body\n").expect("write notes");
    let id = record(
        &bus,
        trigger("handoff", "quaqua/inside", "inside").with_ref("docs/../docs/notes.txt"),
    );

    let recall = load_recall(&bus, id, &fx.root).unwrap().unwrap();
    let detail = recall.ref_detail.as_ref().unwrap();
    assert_eq!(detail.kind, RefKind::Path);
    assert!(detail.exists, "`..` that stays inside the workspace is fine");
    assert_eq!(detail.head, vec!["inside body".to_string()]);
}

#[test]
fn recall_report_ref_extracts_commit_sha() {
    let fx = Fixture::new("report");
    let bus = fx.bus();
    bus.open_thread("quaqua/sude-cancel", "sude-lead")
        .expect("open thread");

    let report = fx.runs().join("sude-cancel.md");
    std::fs::write(
        &report,
        "Outcome: COMPLETE\nSnapshot:        3a3cd55 · quaqua/sude-cancel · ~/Lab/.wt/sude-cancel\n\n## Blockers\nnone\n",
    )
    .expect("write report");

    let id = record(
        &bus,
        trigger("accept", "quaqua/sude-cancel", "branch accepted").with_ref("runs/sude-cancel.md"),
    );
    let recall = load_recall(&bus, id, &fx.root).unwrap().unwrap();
    let detail = recall.ref_detail.as_ref().unwrap();

    assert_eq!(detail.kind, RefKind::Path);
    assert!(detail.exists);
    assert_eq!(detail.commit.as_deref(), Some("3a3cd55"));
    assert!(detail.head.contains(&"Outcome: COMPLETE".to_string()));

    let text = render_text(&recall);
    assert!(text.contains("commit: 3a3cd55"));
    assert!(text.contains("Outcome: COMPLETE"));
}

#[test]
fn recall_json_carries_trigger_thread_and_ref() {
    let fx = Fixture::new("json");
    let bus = fx.bus();
    bus.open_thread("quaqua/sude-cancel", "sude-lead")
        .expect("open thread");

    let report = fx.runs().join("sude-cancel.md");
    std::fs::write(&report, "Snapshot: 3a3cd55 quaqua/sude-cancel\n").expect("write report");

    let id = record(
        &bus,
        trigger("accept", "quaqua/sude-cancel", "branch accepted")
            .with_ref("runs/sude-cancel.md")
            .with_payload_json(r#"{"verdict":"PASS"}"#),
    );
    let recall = load_recall(&bus, id, &fx.root).unwrap().unwrap();
    let value = serde_json::to_value(&recall).expect("serialize recall");

    assert_eq!(value["trigger"]["id"], id);
    assert_eq!(value["trigger"]["payload_json"], r#"{"verdict":"PASS"}"#);
    assert_eq!(value["thread"]["owner"], "sude-lead");
    assert_eq!(value["thread"]["state"], "open");
    assert_eq!(value["thread"]["recent"].as_array().unwrap().len(), 1);
    assert_eq!(value["ref"]["path"], canonical(&report));
    assert_eq!(value["ref"]["kind"], "path");
    assert_eq!(value["ref"]["commit"], "3a3cd55");
    assert_eq!(value["ref"]["truncated"], false);
}

#[test]
fn recall_unknown_id_returns_none() {
    let fx = Fixture::new("unknown");
    let bus = fx.bus();
    assert!(load_recall(&bus, 999, &fx.root).unwrap().is_none());
}

#[test]
fn recall_missing_ref_is_reported_without_following_links() {
    let fx = Fixture::new("missing-ref");
    let bus = fx.bus();
    bus.open_thread("quaqua/x", "lead").expect("open thread");

    let id = record(
        &bus,
        trigger("handoff", "quaqua/x", "gone").with_ref("runs/gone.md"),
    );
    let recall = load_recall(&bus, id, &fx.root).unwrap().unwrap();
    let detail = recall.ref_detail.as_ref().unwrap();
    assert_eq!(detail.kind, RefKind::Path);
    assert!(!detail.exists);
    assert!(detail.head.is_empty());
    assert_eq!(detail.commit, None);
    assert!(render_text(&recall).contains("(missing)"));

    let id = record(
        &bus,
        trigger("handoff", "quaqua/x", "url").with_ref("https://example.com/runs/gone.md"),
    );
    let recall = load_recall(&bus, id, &fx.root).unwrap().unwrap();
    let detail = recall.ref_detail.as_ref().unwrap();
    assert_eq!(detail.kind, RefKind::Scheme);
    assert!(!detail.exists, "an external link is never fetched");
    assert!(
        !render_text(&recall).contains("(missing)"),
        "a URL is not a missing workspace file"
    );
}

#[test]
fn recall_is_read_only_and_repeatable() {
    let fx = Fixture::new("readonly");
    let bus = fx.bus();
    bus.open_thread("quaqua/x", "lead").expect("open thread");
    let id = record(
        &bus,
        trigger("handoff", "quaqua/x", "read me").with_to_name("cos"),
    );

    let before = bus.get_cursor("cos").expect("cursor read");
    let first = load_recall(&bus, id, &fx.root).unwrap().unwrap();
    let second = load_recall(&bus, id, &fx.root).unwrap().unwrap();

    assert_eq!(first, second, "a repeated recall returns the same answer");
    assert_eq!(bus.get_cursor("cos").unwrap(), before);
    assert_eq!(bus.get_cursor("lead").unwrap(), None);
}

/// A shared database holds rows of several farms; recall must answer from the
/// caller's farm only, both for the trigger and for its thread's recent list.
#[test]
fn recall_ignores_triggers_recorded_by_another_farm() {
    let fx = Fixture::new("cross-farm");
    let bus = fx.bus();
    bus.open_thread("quaqua/shared", "lead").expect("open thread");

    let mine = record(&bus, trigger("handoff", "quaqua/shared", "this farm"));
    let theirs = record(
        &bus,
        trigger("handoff", "quaqua/shared", "another farm").with_farm("other-farm"),
    );

    assert!(
        load_recall(&bus, theirs, &fx.root).unwrap().is_none(),
        "a trigger recorded by another farm is not recalled"
    );

    let recall = load_recall(&bus, mine, &fx.root).unwrap().unwrap();
    let recent: Vec<u64> = recall.thread.recent.iter().map(|t| t.id).collect();
    assert_eq!(recent, vec![mine], "the thread's recent list is farm-scoped");
}

/// `quaqua recall` must work on a database it cannot write: the file is
/// read-only and a live writer holds the WAL, so a read-write open, a
/// journal-mode switch or a schema migration would fail or rewrite the file.
#[test]
fn recall_reads_a_read_only_database_without_writing_it() {
    let fx = Fixture::new("ro-cli");
    let bus = fx.bus();
    bus.open_thread("quaqua/x", "lead").expect("open thread");
    let id = record(&bus, trigger("handoff", "quaqua/x", "read only"));

    let db = fx.runs().join("quaqua.db");
    let mut perms = std::fs::metadata(&db).expect("stat db").permissions();
    perms.set_readonly(true);
    std::fs::set_permissions(&db, perms).expect("make db read-only");

    let before = std::fs::read(&db).expect("read db bytes");
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_quaqua"))
        .arg("recall")
        .arg(id.to_string())
        .env("QUAQUA_RUNS_DIR", fx.runs())
        .output()
        .expect("run quaqua recall");

    // Restore before asserting, so a failed assertion still leaves a deletable fixture.
    let mut perms = std::fs::metadata(&db).expect("stat db").permissions();
    perms.set_readonly(false);
    std::fs::set_permissions(&db, perms).expect("restore db permissions");

    assert_eq!(
        out.status.code(),
        Some(0),
        "recall on a read-only database must succeed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("read only"),
        "recall printed the trigger: {stdout}"
    );
    assert_eq!(
        std::fs::read(&db).expect("read db bytes"),
        before,
        "recall leaves the database byte-identical"
    );
    drop(bus);
}

#[test]
fn extract_commit_reads_snapshot_then_verified_lines() {
    assert_eq!(
        extract_commit(
            "Snapshot:        ed44b3fa349329b2aff3044797f32b360fad9433 quaqua/sude-cardci /x"
        )
        .as_deref(),
        Some("ed44b3fa349329b2aff3044797f32b360fad9433")
    );
    assert_eq!(
        extract_commit("Outcome: COMPLETE\nVerified: unit-test-verified a76ea50\n").as_deref(),
        Some("a76ea50")
    );
    assert_eq!(
        extract_commit("Snapshot:        SHA + branch + worktree path"),
        None,
        "the unfilled brief template carries no SHA"
    );
    assert_eq!(extract_commit("## Blockers\nnone"), None);
}
