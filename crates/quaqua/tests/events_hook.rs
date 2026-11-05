//! Tests for `quaqua events hook`, incident notifications, and herdr plugin manifest.

use quaqua::bus::FarmBus;
use quaqua::herdr::Herdr;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn hermetic_temp_dir(prefix: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "quaqua-hook-test-{prefix}-{}-{nanos}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).expect("create hermetic temp dir");
    dir
}
static TEST_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[test]
fn test_herdr_notification_exactly_one_per_incident_key() {
    let runs_dir = hermetic_temp_dir("notif-dedup");
    let _lock = TEST_ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let notif_log = runs_dir.join("notifications.log");

    // Create a stub herdr binary that logs `notification show` calls
    let bin_dir = runs_dir.join("bin");
    fs::create_dir_all(&bin_dir).expect("bin dir");
    let herdr_bin = bin_dir.join("herdr");

    let script = format!(
        r#"#!/bin/sh
if [ "$1" = "notification" ] && [ "$2" = "show" ]; then
    echo "$*" >> "{}"
    exit 0
fi
exit 0
"#,
        notif_log.display()
    );
    fs::write(&herdr_bin, script).expect("write stub herdr");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(&herdr_bin).expect("metadata").permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&herdr_bin, perms).expect("chmod");
    }

    std::env::set_var("HERDR_BIN_PATH", &herdr_bin);

    let event_payload = serde_json::json!({
        "type": "pane_agent_status_changed",
        "pane_id": "pane-abc",
        "agent": "worker-1",
        "agent_status": "blocked",
        "status_since": "12345"
    })
    .to_string();

    let herdr_ops = Herdr::new();

    // Call hook first time
    let res1 = quaqua::bus::events::handle_event_hook(
        &runs_dir,
        Some("pane.agent_status_changed"),
        Some(&event_payload),
        &herdr_ops,
    );
    assert_eq!(
        res1,
        Ok(true),
        "first hook call must record incident and notify"
    );

    // Verify 1 notification logged
    let log1 = fs::read_to_string(&notif_log).unwrap_or_default();
    let count1 = log1
        .lines()
        .filter(|l| l.contains("notification show"))
        .count();
    assert_eq!(count1, 1, "exactly 1 notification on first call");

    // Call hook second time with the exact same payload / incident_key
    let res2 = quaqua::bus::events::handle_event_hook(
        &runs_dir,
        Some("pane.agent_status_changed"),
        Some(&event_payload),
        &herdr_ops,
    );
    assert_eq!(
        res2,
        Ok(false),
        "second hook call must recognize duplicate and return false"
    );

    // Verify notification count is STILL exactly 1
    let log2 = fs::read_to_string(&notif_log).unwrap_or_default();
    let count2 = log2
        .lines()
        .filter(|l| l.contains("notification show"))
        .count();
    assert_eq!(
        count2, 1,
        "must NOT send duplicate notification on repeated hook call"
    );

    // Call hook third time
    let res3 = quaqua::bus::events::handle_event_hook(
        &runs_dir,
        Some("pane.agent_status_changed"),
        Some(&event_payload),
        &herdr_ops,
    );
    assert_eq!(res3, Ok(false));
    let log3 = fs::read_to_string(&notif_log).unwrap_or_default();
    let count3 = log3
        .lines()
        .filter(|l| l.contains("notification show"))
        .count();
    assert_eq!(
        count3, 1,
        "must remain exactly 1 notification across 3 calls"
    );

    // A different incident key (different status_since or pane) should trigger a new notification
    let event_payload_2 = serde_json::json!({
        "type": "pane_agent_status_changed",
        "pane_id": "pane-def",
        "agent": "worker-2",
        "agent_status": "blocked",
        "status_since": "99999"
    })
    .to_string();

    let res4 = quaqua::bus::events::handle_event_hook(
        &runs_dir,
        Some("pane.agent_status_changed"),
        Some(&event_payload_2),
        &herdr_ops,
    );
    assert_eq!(res4, Ok(true), "different incident key must notify");
    let log4 = fs::read_to_string(&notif_log).unwrap_or_default();
    let count4 = log4
        .lines()
        .filter(|l| l.contains("notification show"))
        .count();
    assert_eq!(
        count4, 2,
        "must have exactly 2 notifications total (1 per incident key)"
    );
}

#[test]
fn test_plugin_manifest_contains_events_hook() {
    let manifest_path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .expect("repo root")
        .join("plugins/herdr-quaqua/herdr-plugin.toml");

    let text = fs::read_to_string(&manifest_path).expect("read herdr-plugin.toml");

    assert!(
        text.contains("command = [\"quaqua\", \"events\", \"hook\"]"),
        "herdr-plugin.toml must declare quaqua events hook command"
    );
    assert!(
        text.contains("on = \"pane.agent_status_changed\""),
        "herdr-plugin.toml must subscribe to pane.agent_status_changed"
    );
    assert!(
        text.contains("on = \"pane.exited\""),
        "herdr-plugin.toml must subscribe to pane.exited"
    );
}

#[test]
fn test_finding_1_blocked_unique_per_episode_without_status_since() {
    let runs_dir = hermetic_temp_dir("finding-1-episode");
    let _lock = TEST_ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let notif_log = runs_dir.join("notifications.log");

    let bin_dir = runs_dir.join("bin");
    fs::create_dir_all(&bin_dir).expect("bin dir");
    let herdr_bin = bin_dir.join("herdr");

    let script = format!(
        r#"#!/bin/sh
if [ "$1" = "notification" ] && [ "$2" = "show" ]; then
    echo "$*" >> "{}"
    exit 0
fi
exit 0
"#,
        notif_log.display()
    );
    fs::write(&herdr_bin, script).expect("write stub herdr");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(&herdr_bin).expect("metadata").permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&herdr_bin, perms).expect("chmod");
    }

    std::env::set_var("HERDR_BIN_PATH", &herdr_bin);
    let herdr_ops = Herdr::new();

    // Episode 1: Pane w9:p6 blocks (no status_since in payload)
    let payload_blocked_1 = serde_json::json!({
        "pane_id": "w9:p6",
        "agent": "worker-1",
        "agent_status": "blocked",
        "workspace_id": "w9"
    })
    .to_string();

    let res1 = quaqua::bus::events::handle_event_hook(
        &runs_dir,
        Some("pane.agent_status_changed"),
        Some(&payload_blocked_1),
        &herdr_ops,
    );
    assert_eq!(res1, Ok(true), "episode 1 must notify");

    // Repeated delivery within episode 1 must be deduped
    let res1_dup = quaqua::bus::events::handle_event_hook(
        &runs_dir,
        Some("pane.agent_status_changed"),
        Some(&payload_blocked_1),
        &herdr_ops,
    );
    assert_eq!(
        res1_dup,
        Ok(false),
        "repeated delivery in episode 1 must be deduped"
    );

    let count1 = fs::read_to_string(&notif_log)
        .unwrap_or_default()
        .lines()
        .filter(|l| l.contains("notification show"))
        .count();
    assert_eq!(count1, 1, "exactly 1 notification after episode 1");

    // Unblock: worker transitions to working
    let payload_working = serde_json::json!({
        "pane_id": "w9:p6",
        "agent": "worker-1",
        "agent_status": "working",
        "workspace_id": "w9"
    })
    .to_string();
    let _ = quaqua::bus::events::handle_event_hook(
        &runs_dir,
        Some("pane.agent_status_changed"),
        Some(&payload_working),
        &herdr_ops,
    );

    // Episode 2: Worker blocks again! Real payload shape, no status_since
    let res2 = quaqua::bus::events::handle_event_hook(
        &runs_dir,
        Some("pane.agent_status_changed"),
        Some(&payload_blocked_1),
        &herdr_ops,
    );
    assert_eq!(res2, Ok(true), "episode 2 must notify after unblocking");

    let count2 = fs::read_to_string(&notif_log)
        .unwrap_or_default()
        .lines()
        .filter(|l| l.contains("notification show"))
        .count();
    assert_eq!(
        count2, 2,
        "second blocked episode must send a second notification"
    );

    // Missing pane_id: two different agents with empty pane_id must not collapse farm-wide
    let payload_nopane_a = serde_json::json!({
        "pane_id": "",
        "agent": "agent-A",
        "agent_status": "blocked",
        "workspace_id": "w9"
    })
    .to_string();
    let res_a = quaqua::bus::events::handle_event_hook(
        &runs_dir,
        Some("pane.agent_status_changed"),
        Some(&payload_nopane_a),
        &herdr_ops,
    );
    assert_eq!(res_a, Ok(true), "first pane-less agent must notify");

    let payload_nopane_b = serde_json::json!({
        "pane_id": "",
        "agent": "agent-B",
        "agent_status": "blocked",
        "workspace_id": "w9"
    })
    .to_string();
    let res_b = quaqua::bus::events::handle_event_hook(
        &runs_dir,
        Some("pane.agent_status_changed"),
        Some(&payload_nopane_b),
        &herdr_ops,
    );
    assert_eq!(
        res_b,
        Ok(true),
        "second pane-less agent must notify, not collapse into blocked:"
    );
}

#[test]
fn test_finding_4_events_hook_hard_timeout_on_hanging_notification() {
    let runs_dir = hermetic_temp_dir("finding-4-timeout");
    let bin_dir = runs_dir.join("bin");
    fs::create_dir_all(&bin_dir).expect("bin dir");
    let herdr_bin = bin_dir.join("herdr");
    let _lock = TEST_ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());

    // Stub herdr that hangs for 10 seconds
    let script = r#"#!/bin/sh
if [ "$1" = "notification" ] && [ "$2" = "show" ]; then
    sleep 10
    exit 0
fi
exit 0
"#;
    fs::write(&herdr_bin, script).expect("write stub herdr");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(&herdr_bin).expect("metadata").permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&herdr_bin, perms).expect("chmod");
    }

    let quaqua_bin = env!("CARGO_BIN_EXE_quaqua");
    let event_payload = serde_json::json!({
        "type": "pane_agent_status_changed",
        "pane_id": "pane-hang",
        "agent": "worker-hang",
        "agent_status": "blocked"
    })
    .to_string();

    let start = std::time::Instant::now();
    let output = Command::new(quaqua_bin)
        .args(["events", "hook"])
        .env("HERDR_BIN_PATH", &herdr_bin)
        .env("QUAQUA_RUNS_DIR", &runs_dir)
        .env("HERDR_PLUGIN_EVENT", "pane.agent_status_changed")
        .env("HERDR_PLUGIN_EVENT_JSON", &event_payload)
        .output()
        .expect("run events hook");
    let elapsed = start.elapsed();

    assert_eq!(
        output.status.code(),
        Some(0),
        "hook must exit 0 within deadline"
    );
    assert!(
        elapsed < std::time::Duration::from_millis(2500),
        "hook must finish within hard timeout (<2.5s) even if herdr hangs, took {:?}",
        elapsed
    );
}

#[test]
fn test_events_hook_drops_foreign_pane_incidents() {
    let runs_dir = hermetic_temp_dir("foreign-pane");
    let _lock = TEST_ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let notif_log = runs_dir.join("notifications.log");

    let bin_dir = runs_dir.join("bin");
    fs::create_dir_all(&bin_dir).expect("bin dir");
    let herdr_bin = bin_dir.join("herdr");

    let script = format!(
        r#"#!/bin/sh
if [ "$1" = "pane" ] && [ "$2" = "get" ]; then
    if [ "$3" = "w7:p2" ]; then
        echo '{{"id":"cli:pane:get","result":{{"pane":{{"cwd":"/other/TTDVKH","tokens":{{}}}}}}}}'
        exit 0
    elif [ "$3" = "w8:p1" ]; then
        echo '{{"error":{{"code":"pane_not_found","message":"pane not found"}}}}' >&2
        exit 1
    fi
fi
if [ "$1" = "notification" ] && [ "$2" = "show" ]; then
    echo "$*" >> "{}"
    exit 0
fi
exit 0
"#,
        notif_log.display()
    );
    fs::write(&herdr_bin, script).expect("write stub herdr");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(&herdr_bin).expect("metadata").permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&herdr_bin, perms).expect("chmod");
    }

    std::env::set_var("HERDR_BIN_PATH", &herdr_bin);
    let herdr_ops = Herdr::new();

    // 1. Foreign pane w7:p2 (foreign cwd, no grid_* tokens) blocks
    let payload_foreign = serde_json::json!({
        "type": "pane_agent_status_changed",
        "pane_id": "w7:p2",
        "agent": "ttdvkh-cos",
        "agent_status": "blocked",
        "status_since": "1793173517554",
        "tokens": {
            "logo": "\u{e1a3}"
        }
    })
    .to_string();

    let res1 = quaqua::bus::events::handle_event_hook(
        &runs_dir,
        Some("pane.agent_status_changed"),
        Some(&payload_foreign),
        &herdr_ops,
    );
    assert_eq!(
        res1,
        Ok(false),
        "foreign pane incident must be dropped at ingest"
    );

    let db_path = runs_dir.join("quaqua.db");
    if let Ok(bus) = FarmBus::open(&db_path) {
        let events = bus.load_events(0).unwrap_or_default();
        assert!(
            events.is_empty(),
            "foreign pane must not record any row in triggers"
        );
    }

    // 2. Dead pane w8:p1 (pane_not_found)
    let payload_dead = serde_json::json!({
        "type": "pane_agent_status_changed",
        "pane_id": "w8:p1",
        "agent": "dead-worker",
        "agent_status": "blocked",
        "status_since": "1793173517555"
    })
    .to_string();

    let res2 = quaqua::bus::events::handle_event_hook(
        &runs_dir,
        Some("pane.agent_status_changed"),
        Some(&payload_dead),
        &herdr_ops,
    );
    assert_eq!(res2, Ok(false), "dead pane must be dropped as foreign");

    let notif = fs::read_to_string(&notif_log).unwrap_or_default();
    assert_eq!(
        notif.lines().count(),
        0,
        "no notification should be sent for foreign or dead panes"
    );

    // Neither pane may reach a consumer digest either.
    if let Ok(bus) = FarmBus::open(&db_path) {
        let digest = bus.inbox_digest(&runs_dir, "cos").expect("digest");
        assert_eq!(digest.incidents.len(), 0, "no incident recorded in DB");
    }
}

#[test]
fn test_events_hook_fails_open_when_pane_get_unparseable() {
    let runs_dir = hermetic_temp_dir("fail-open");
    let _lock = TEST_ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let notif_log = runs_dir.join("notifications.log");

    let bin_dir = runs_dir.join("bin");
    fs::create_dir_all(&bin_dir).expect("bin dir");
    let herdr_bin = bin_dir.join("herdr");

    let script = format!(
        r#"#!/bin/sh
if [ "$1" = "pane" ] && [ "$2" = "get" ]; then
    echo "internal server error: timeout" >&2
    exit 1
fi
if [ "$1" = "notification" ] && [ "$2" = "show" ]; then
    echo "$*" >> "{}"
    exit 0
fi
exit 0
"#,
        notif_log.display()
    );
    fs::write(&herdr_bin, script).expect("write stub herdr");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(&herdr_bin).expect("metadata").permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&herdr_bin, perms).expect("chmod");
    }

    std::env::set_var("HERDR_BIN_PATH", &herdr_bin);
    let herdr_ops = Herdr::new();

    let payload = serde_json::json!({
        "type": "pane_agent_status_changed",
        "pane_id": "w1:pA9",
        "agent": "my-worker",
        "agent_status": "blocked",
        "status_since": "1793173517999"
    })
    .to_string();

    let res = quaqua::bus::events::handle_event_hook(
        &runs_dir,
        Some("pane.agent_status_changed"),
        Some(&payload),
        &herdr_ops,
    );
    assert_eq!(
        res,
        Ok(true),
        "must fail open and notify when herdr pane get fails"
    );
}

#[test]
fn test_events_hook_stamps_owner_project_farm_from_pane_tokens() {
    let runs_dir = hermetic_temp_dir("tokens-stamp");
    let _lock = TEST_ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let notif_log = runs_dir.join("notifications.log");

    let bin_dir = runs_dir.join("bin");
    fs::create_dir_all(&bin_dir).expect("bin dir");
    let herdr_bin = bin_dir.join("herdr");

    let db_path = runs_dir.join("quaqua.db");
    let bus = FarmBus::open(&db_path).expect("open bus");
    let local_farm = bus.farm().to_string();

    let script = format!(
        r#"#!/bin/sh
if [ "$1" = "pane" ] && [ "$2" = "get" ]; then
    if [ "$3" = "w1:pA8" ]; then
        echo '{{"id":"cli:pane:get","result":{{"pane":{{"cwd":"/other/path","tokens":{{"grid_farm":"{local_farm}","grid_owner":"mu-lead","grid_group":"myproj"}}}}}}}}'
        exit 0
    fi
fi
if [ "$1" = "notification" ] && [ "$2" = "show" ]; then
    echo "$*" >> "{}"
    exit 0
fi
exit 0
"#,
        notif_log.display()
    );
    fs::write(&herdr_bin, script).expect("write stub herdr");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(&herdr_bin).expect("metadata").permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&herdr_bin, perms).expect("chmod");
    }

    std::env::set_var("HERDR_BIN_PATH", &herdr_bin);
    let herdr_ops = Herdr::new();

    let payload = serde_json::json!({
        "type": "pane_agent_status_changed",
        "pane_id": "w1:pA8",
        "agent": "worker-lead",
        "agent_status": "blocked",
        "status_since": "1793173518000"
    })
    .to_string();

    let res = quaqua::bus::events::handle_event_hook(
        &runs_dir,
        Some("pane.agent_status_changed"),
        Some(&payload),
        &herdr_ops,
    );
    assert_eq!(res, Ok(true));

    let events = bus.load_events(0).expect("load events");
    assert_eq!(events.len(), 1);
    let ev = &events[0];
    assert_eq!(ev.owner.as_deref(), Some("mu-lead"));
    assert_eq!(ev.project.as_deref(), Some("myproj"));
    assert_eq!(ev.farm.as_deref(), Some(local_farm.as_str()));
}

#[test]
fn test_events_hook_drops_foreign_cos_pane_to_prevent_cross_cos_inbox_leak() {
    let runs_dir = hermetic_temp_dir("foreign-cos-hook");
    let _lock = TEST_ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let notif_log = runs_dir.join("notifications.log");

    let bin_dir = runs_dir.join("bin");
    fs::create_dir_all(&bin_dir).expect("bin dir");
    let herdr_bin = bin_dir.join("herdr");

    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
    let foreign_cwd = format!("{home}/Work/TTDVKH");
    let db_path = runs_dir.join("quaqua.db");
    let bus = FarmBus::open(&db_path).expect("open bus");
    let local_farm = bus.farm().to_string();
    let script = format!(
        r#"#!/bin/sh
if [ "$1" = "notification" ] && [ "$2" = "show" ]; then
    echo "$*" >> "{}"
    exit 0
fi
if [ "$1" = "pane" ] && [ "$2" = "get" ] && [ "$3" = "w7:p2" ]; then
    printf '{{"result":{{"pane":{{"pane_id":"w7:p2","cwd":"{}","tokens":{{"grid_role":"cos","grid_owner":"cos","grid_farm":"{local_farm}","display_agent":"CoS TTDVKH"}}}}}}}}'
    exit 0
fi
exit 0
"#,
        notif_log.display(),
        foreign_cwd
    );
    fs::write(&herdr_bin, script).expect("write stub herdr");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(&herdr_bin).expect("metadata").permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&herdr_bin, perms).expect("chmod");
    }

    std::env::set_var("HERDR_BIN_PATH", &herdr_bin);

    // Foreign CoS pane from another workspace (w7:p2) with grid_role=cos. It carries this
    // farm's token, so only the workspace check can keep it out of this inbox.
    let foreign_cos_payload = serde_json::json!({
        "type": "pane_agent_status_changed",
        "pane_id": "w7:p2",
        "agent": "claude",
        "agent_status": "blocked",
        "tokens": {
            "grid_role": "cos",
            "grid_owner": "cos",
            "grid_farm": local_farm,
            "display_agent": "CoS TTDVKH"
        }
    })
    .to_string();

    let herdr_ops = Herdr::new();
    let res = quaqua::bus::events::handle_event_hook(
        &runs_dir,
        Some("pane.agent_status_changed"),
        Some(&foreign_cos_payload),
        &herdr_ops,
    );
    assert_eq!(
        res,
        Ok(false),
        "foreign CoS pane must be dropped and return Ok(false)"
    );

    let log = fs::read_to_string(&notif_log).unwrap_or_default();
    assert!(
        log.is_empty(),
        "must not send desktop notification for foreign CoS pane: {log}"
    );

    // Verify nothing recorded in DB
    let digest = bus.inbox_digest(&runs_dir, "cos").expect("digest");
    assert_eq!(digest.incidents.len(), 0, "no incident recorded in DB");
}

#[test]
fn test_is_pane_in_runs_workspace_treats_factory_workspace_parent_as_local() {
    // A `.cos` install records runs under `<workspace>/.cos/runs`, and its CoS pane sits at
    // `<workspace>`. Both must count as this workspace; anything else must not.
    let root = hermetic_temp_dir("factory-ws");
    let ws = root.join("ttdvkh");
    let runs = ws.join(".cos/runs");
    fs::create_dir_all(&runs).expect("cos runs dir");
    let other = root.join("other");
    fs::create_dir_all(&other).expect("other workspace");

    assert!(
        quaqua::bus::events::is_pane_in_runs_workspace(
            ws.to_str().unwrap(),
            ws.to_str(),
            &runs,
            None
        ),
        "the workspace root of a .cos install belongs to the workspace"
    );
    assert!(
        quaqua::bus::events::is_pane_in_runs_workspace("w1:p2", other.to_str(), &runs, None)
            == false,
        "a pane from another workspace is not local"
    );
}
