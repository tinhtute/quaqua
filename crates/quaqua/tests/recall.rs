//! Tests for `quaqua recall <id>`: trigger → thread → ref, at most two hops.

use quaqua::bus::recall::{extract_commit, load_recall, render_text, REF_HEAD_LINES};
use quaqua::bus::{FarmBus, NewTrigger};
use std::path::PathBuf;

struct Fixture {
    root: PathBuf,
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
        Self { root }
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

    assert_eq!(detail.path, notes.display().to_string());
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
    assert!(text.contains(&format!("ref {}", notes.display())));
    assert!(text.contains("Snapshot: 3a3cd55"));
    assert!(text.contains("line 38"));
    assert!(
        !text.contains("line 39"),
        "line 41 is past the 40-line head"
    );
    assert!(text.contains("(truncated)"));
}

#[test]
fn recall_absolute_ref_is_read_as_is() {
    let fx = Fixture::new("abs-ref");
    let bus = fx.bus();
    bus.open_thread("quaqua/abs", "lead").expect("open thread");

    let outside = fx.root.join("abs.txt");
    std::fs::write(&outside, "absolute ref body\n").expect("write abs file");
    let id = record(
        &bus,
        trigger("handoff", "quaqua/abs", "abs").with_ref(outside.to_str().unwrap()),
    );

    let recall = load_recall(&bus, id, &fx.root).unwrap().unwrap();
    let detail = recall.ref_detail.as_ref().unwrap();
    assert!(detail.exists);
    assert_eq!(detail.head, vec!["absolute ref body".to_string()]);
    assert!(!detail.truncated);
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
    assert_eq!(value["ref"]["path"], report.display().to_string());
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
    assert!(!detail.exists);
    assert!(detail.head.is_empty());
    assert_eq!(detail.commit, None);
    assert!(render_text(&recall).contains("(missing)"));

    let id = record(
        &bus,
        trigger("handoff", "quaqua/x", "url").with_ref("https://example.com/runs/gone.md"),
    );
    let recall = load_recall(&bus, id, &fx.root).unwrap().unwrap();
    assert!(
        !recall.ref_detail.as_ref().unwrap().exists,
        "an external link is never fetched"
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
