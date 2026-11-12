mod common;
use common::FakeHerdrDriver;
use quaqua::grid::hook::{compute_snapshot_signature, reconcile_step};
use quaqua::grid::{
    apply_ops, parse_snapshot_value, plan, run_reconcile, HerdrDriver, MoveResult,
    ReconcileOptions, Snapshot, Target,
};
use std::fs;
use std::path::PathBuf;
use std::process::Command;

/// Reconcile options for an owner-wide repair (`grid reconcile --owner X --tab T`).
fn repair_opts(owner: &str, tab: &str) -> ReconcileOptions {
    ReconcileOptions {
        check: false,
        repair: true,
        tab: Some(tab.to_string()),
        only: None,
        owner: Some(owner.to_string()),
        dry_run: false,
        timeout_sec: None,
    }
}

/// The pane id the driver now uses for the worker labelled `label`.
fn pane_id_by_label(driver: &FakeHerdrDriver, label: &str) -> String {
    driver
        .current_snapshot
        .panes
        .iter()
        .find(|p| p.label.as_deref() == Some(label))
        .map(|p| p.pane_id.clone())
        .unwrap_or_else(|| panic!("no pane labelled {label}"))
}

fn idle_the_working_panes(driver: &mut FakeHerdrDriver) {
    for p in &mut driver.current_snapshot.panes {
        if p.agent_status.as_deref() == Some("working") {
            p.agent_status = Some("idle".to_string());
        }
    }
}

fn tab_labelled(driver: &FakeHerdrDriver, label: &str) -> Option<String> {
    driver
        .current_snapshot
        .tabs
        .iter()
        .find(|t| t.label.as_deref() == Some(label))
        .map(|t| t.tab_id.clone())
}

fn panes_in(driver: &FakeHerdrDriver, tab_id: &str) -> Vec<String> {
    driver
        .current_snapshot
        .panes
        .iter()
        .filter(|p| p.tab_id.as_deref() == Some(tab_id))
        .map(|p| p.pane_id.clone())
        .collect()
}

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/herdr-quaqua")
}

fn plan_jq_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../plugins/herdr-quaqua/plan.jq")
}

#[test]
fn test_0_workers_yields_empty_ops() {
    let fix = fixtures_dir().join("0-workers.json");
    let content = fs::read_to_string(fix).unwrap();
    let val: serde_json::Value = serde_json::from_str(&content).unwrap();
    let snap = parse_snapshot_value(&val).unwrap();
    let ops = plan(&snap).unwrap();
    assert_eq!(ops, vec![]);
}

#[test]
fn test_1_worker_split_beside_anchor() {
    let fix = fixtures_dir().join("1-worker.json");
    let content = fs::read_to_string(fix).unwrap();
    let val: serde_json::Value = serde_json::from_str(&content).unwrap();
    let snap = parse_snapshot_value(&val).unwrap();
    let ops = plan(&snap).unwrap();
    assert_eq!(ops.len(), 1);
    assert_eq!(ops[0].pane, "w1:p5");
    assert!(!ops[0].deferred);
    match &ops[0].target {
        Target::Split {
            tab,
            anchor,
            direction,
            ratio,
        } => {
            assert_eq!(tab, "w1:t1");
            assert_eq!(anchor, "w1:p1");
            assert_eq!(direction, "right");
            assert_eq!(*ratio, 0.4);
        }
        _ => panic!("expected split target"),
    }
}

#[test]
fn test_2_workers_lands_in_grid_tab_never_orchestrator_tab() {
    let fix = fixtures_dir().join("2-workers.json");
    let content = fs::read_to_string(fix).unwrap();
    let val: serde_json::Value = serde_json::from_str(&content).unwrap();
    let snap = parse_snapshot_value(&val).unwrap();
    let ops = plan(&snap).unwrap();
    assert_eq!(ops.len(), 2);
    // op 0: new_tab quaqua-workers (GRID_TAB, not orchestrator tab w1:t1)
    match &ops[0].target {
        Target::NewTab { label } => {
            assert_eq!(label, "quaqua-workers");
        }
        _ => panic!("op 0 expected new_tab, got {:?}", ops[0].target),
    }
    // op 1: split on @workers
    match &ops[1].target {
        Target::Split {
            tab,
            anchor,
            direction,
            ratio,
        } => {
            assert_eq!(tab, "@workers");
            assert_eq!(anchor, "w1:p5");
            assert_eq!(direction, "right");
            assert_eq!(*ratio, 0.5);
        }
        _ => panic!("op 1 expected split, got {:?}", ops[1].target),
    }
}

#[test]
fn test_5_workers_overflows_to_workers_2() {
    let fix = fixtures_dir().join("5-workers.json");
    let content = fs::read_to_string(fix).unwrap();
    let val: serde_json::Value = serde_json::from_str(&content).unwrap();
    let snap = parse_snapshot_value(&val).unwrap();
    let ops = plan(&snap).unwrap();
    assert_eq!(ops.len(), 5);
    // First 4 workers land in quaqua-workers (2x2 grid, cell order)
    assert_eq!(ops[0].pane, "w1:p5");
    assert_eq!(
        ops[0].target,
        Target::NewTab {
            label: "quaqua-workers".to_string()
        }
    );
    assert_eq!(ops[1].pane, "w1:p6");
    assert_eq!(
        ops[1].target,
        Target::Split {
            tab: "@workers".to_string(),
            anchor: "w1:p5".to_string(),
            direction: "right".to_string(),
            ratio: 0.5
        }
    );
    assert_eq!(ops[2].pane, "w1:p7");
    assert_eq!(
        ops[2].target,
        Target::Split {
            tab: "@workers".to_string(),
            anchor: "w1:p6".to_string(),
            direction: "down".to_string(),
            ratio: 0.5
        }
    );
    assert_eq!(ops[3].pane, "w1:p8");
    assert_eq!(
        ops[3].target,
        Target::Split {
            tab: "@workers".to_string(),
            anchor: "w1:p5".to_string(),
            direction: "down".to_string(),
            ratio: 0.5
        }
    );
    // 5th worker overflows into quaqua-workers-2
    assert_eq!(ops[4].pane, "w1:p9");
    assert_eq!(
        ops[4].target,
        Target::NewTab {
            label: "quaqua-workers-2".to_string()
        }
    );
}

#[test]
fn test_8_workers_grid_two_tabs_of_four() {
    let fix = fixtures_dir().join("8-workers.json");
    let content = fs::read_to_string(fix).unwrap();
    let val: serde_json::Value = serde_json::from_str(&content).unwrap();
    let snap = parse_snapshot_value(&val).unwrap();
    let ops = plan(&snap).unwrap();
    assert_eq!(ops.len(), 8);
    // Chunk 0: 4 workers in quaqua-workers (2x2, cell order A B / D C)
    assert_eq!(ops[0].pane, "w1:p5");
    assert_eq!(
        ops[0].target,
        Target::NewTab {
            label: "quaqua-workers".to_string()
        }
    );
    assert_eq!(ops[1].pane, "w1:p6");
    assert_eq!(
        ops[1].target,
        Target::Split {
            tab: "@workers".to_string(),
            anchor: "w1:p5".to_string(),
            direction: "right".to_string(),
            ratio: 0.5
        }
    );
    assert_eq!(ops[2].pane, "w1:p7");
    assert_eq!(
        ops[2].target,
        Target::Split {
            tab: "@workers".to_string(),
            anchor: "w1:p6".to_string(),
            direction: "down".to_string(),
            ratio: 0.5
        }
    );
    assert_eq!(ops[3].pane, "w1:p8");
    assert_eq!(
        ops[3].target,
        Target::Split {
            tab: "@workers".to_string(),
            anchor: "w1:p5".to_string(),
            direction: "down".to_string(),
            ratio: 0.5
        }
    );
    // Chunk 1: 4 workers in quaqua-workers-2 (2x2, cell order)
    assert_eq!(ops[4].pane, "w1:p9");
    assert_eq!(
        ops[4].target,
        Target::NewTab {
            label: "quaqua-workers-2".to_string()
        }
    );
    assert_eq!(ops[5].pane, "w1:pA");
    assert_eq!(
        ops[5].target,
        Target::Split {
            tab: "@workers-2".to_string(),
            anchor: "w1:p9".to_string(),
            direction: "right".to_string(),
            ratio: 0.5
        }
    );
    assert_eq!(ops[6].pane, "w1:pB");
    assert_eq!(
        ops[6].target,
        Target::Split {
            tab: "@workers-2".to_string(),
            anchor: "w1:pA".to_string(),
            direction: "down".to_string(),
            ratio: 0.5
        }
    );
    assert_eq!(ops[7].pane, "w1:pC");
    assert_eq!(
        ops[7].target,
        Target::Split {
            tab: "@workers-2".to_string(),
            anchor: "w1:p9".to_string(),
            direction: "down".to_string(),
            ratio: 0.5
        }
    );
}

#[test]
fn test_9_workers_rejected_with_error() {
    let fix = fixtures_dir().join("8-workers.json");
    let content = fs::read_to_string(fix).unwrap();
    let mut val: serde_json::Value = serde_json::from_str(&content).unwrap();
    {
        let panes = val["snapshot"]["panes"].as_array_mut().unwrap();
        panes.push(serde_json::json!({
            "pane_id": "w1:pD",
            "tab_id": "w1:t2",
            "workspace_id": "w1",
            "agent": "pi",
            "agent_status": "idle",
            "tokens": {
                "grid_role": "worker",
                "grid_owner": "cos",
                "grid_group": "quaqua-workers",
                "grid_ready": "1"
            }
        }));
    }
    let snap = parse_snapshot_value(&val).unwrap();
    let err = plan(&snap).unwrap_err();
    assert!(
        err.contains("too many workers in group quaqua-workers: 9 (max 8)"),
        "unexpected error message: {err}"
    );
}

#[test]
fn test_oracle_parity_with_plan_jq_on_panes_and_tabs() {
    // plan.jq is the retired bash planner kept as an oracle. It pins the old
    // 3-pane layout (A|B top + C below); issue #79 replaced it with A │ B/C,
    // so the comparison covers everything except the split geometry that the
    // decision changed: which panes move, their deferral, and which tab they
    // land in. The target shape itself is pinned by the unit tests below.
    let jq_bin = Command::new("which")
        .arg("jq")
        .output()
        .ok()
        .filter(|o| o.status.success());

    if jq_bin.is_none() {
        eprintln!("SKIP: jq not found on PATH; skipping differential comparison against plan.jq");
        return;
    }

    let plan_jq = plan_jq_path();
    assert!(plan_jq.exists(), "missing plan.jq at {:?}", plan_jq);

    let entries = fs::read_dir(fixtures_dir()).unwrap();
    let mut count = 0;

    // Everything the oracle still owns: candidate filtering, tab choice,
    // chunking, deferral. Not a re-placement inside a tab the pane already
    // sits in — that is the issue #79 shape decision.
    fn landing(
        ops: &serde_json::Value,
        panes: &[quaqua::grid::types::Pane],
    ) -> Vec<serde_json::Value> {
        let mut out: Vec<serde_json::Value> = ops
            .as_array()
            .unwrap()
            .iter()
            .filter(|op| {
                let pane = op["pane"].as_str().unwrap_or("");
                let tab = op["target"]["tab"].as_str();
                !panes
                    .iter()
                    .any(|p| p.pane_id == pane && p.tab_id.as_deref() == tab)
            })
            .map(|op| {
                serde_json::json!({
                    "op": op["op"],
                    "pane": op["pane"],
                    "deferred": op["deferred"],
                    "kind": op["target"]["kind"],
                    "label": op["target"]["label"],
                    "tab": op["target"]["tab"],
                })
            })
            .collect();
        // Placement order inside a tab is the issue #79 decision; parity is per
        // pane (who lands where), not the sequence.
        out.sort_by_key(|op| op["pane"].as_str().unwrap_or("").to_string());
        out
    }

    for entry in entries {
        let path = entry.unwrap().path();
        if path.extension().and_then(|s| s.to_str()) != Some("json") {
            continue;
        }

        let content = fs::read_to_string(&path).unwrap();
        let val: serde_json::Value = serde_json::from_str(&content).unwrap();
        let snap = parse_snapshot_value(&val)
            .unwrap_or_else(|e| panic!("failed to parse fixture {}: {e}", path.display()));
        let rust_res = plan(&snap);

        let jq_out = Command::new("jq")
            .arg("-c")
            .arg("-f")
            .arg(&plan_jq)
            .arg(&path)
            .output()
            .unwrap();

        let filename = path.file_name().unwrap().to_string_lossy();

        if !jq_out.status.success() {
            // Both must fail
            assert!(
                rust_res.is_err(),
                "fixture {filename}: plan.jq failed but Rust plan succeeded"
            );
            let jq_err = String::from_utf8_lossy(&jq_out.stderr);
            let rust_err = rust_res.unwrap_err();
            assert!(
                rust_err.contains("too many workers in group")
                    && jq_err.contains("too many workers in group"),
                "fixture {filename}: error mismatch: rust='{rust_err}', jq='{jq_err}'"
            );
        } else {
            assert!(
                rust_res.is_ok(),
                "fixture {filename}: plan.jq succeeded but Rust plan failed: {:?}",
                rust_res.err()
            );
            let rust_ops = rust_res.unwrap();
            let rust_json = serde_json::to_value(&rust_ops).unwrap();
            let jq_json: serde_json::Value =
                serde_json::from_slice(&jq_out.stdout).unwrap_or(serde_json::Value::Null);

            assert_eq!(
                landing(&rust_json, &snap.panes),
                landing(&jq_json, &snap.panes),
                "fixture {filename}: pane/tab parity with plan.jq broke\nRust:\n{}\njq:\n{}",
                serde_json::to_string_pretty(&rust_json).unwrap(),
                serde_json::to_string_pretty(&jq_json).unwrap(),
            );
        }
        count += 1;
    }

    assert!(
        count >= 23,
        "expected at least 23 fixtures tested, got {count}"
    );
    println!("Pane/tab parity passed for all {count} fixtures against plan.jq");
}

#[test]
fn test_reconcile_only_with_custom_tab_converges_immediately() {
    // dec5a30 bugfix test:
    // only-custom-tab.json has workers with grid_group="quaqua".
    // When --tab mono-workers is specified, ops whose group is not mono-workers
    // are ignored, so remaining ops is empty and it converges immediately (exit 0).
    let fix = fixtures_dir().join("only-custom-tab.json");
    let content = fs::read_to_string(fix).unwrap();
    let val: serde_json::Value = serde_json::from_str(&content).unwrap();
    let snap = parse_snapshot_value(&val).unwrap();

    let mut driver = FakeHerdrDriver::new(snap);
    let opts = ReconcileOptions {
        check: true,
        repair: false,
        tab: Some("mono-workers".to_string()),
        only: Some("sude-isl,suhuynh-islr1".to_string()),
        owner: Some("cos".to_string()),
        dry_run: true,
        timeout_sec: Some(0),
    };

    let res = run_reconcile(&mut driver, &opts, "workers", None);
    assert!(
        res.is_ok(),
        "custom tab mono-workers must converge immediately: {:?}",
        res
    );

    // Conversely, with matching tab quaqua-workers, pending ops must be found
    let opts_matching = ReconcileOptions {
        check: true,
        repair: false,
        tab: Some("quaqua-workers".to_string()),
        only: Some("sude-isl,suhuynh-islr1".to_string()),
        owner: Some("cos".to_string()),
        dry_run: true,
        timeout_sec: Some(0),
    };
    let res_matching = run_reconcile(&mut driver, &opts_matching, "workers", None);
    assert!(
        res_matching.is_err(),
        "matching tab quaqua-workers must report pending ops"
    );
}

#[test]
fn test_fake_herdr_driver_applies_ops_cleanly() {
    let fix = fixtures_dir().join("2-workers.json");
    let content = fs::read_to_string(fix).unwrap();
    let val: serde_json::Value = serde_json::from_str(&content).unwrap();
    let snap = parse_snapshot_value(&val).unwrap();

    let ops = plan(&snap).unwrap();
    assert_eq!(ops.len(), 2);
    let mut driver = FakeHerdrDriver::new(snap.clone());
    let pane_map = apply_ops(&mut driver, &ops, Some(&snap)).unwrap();
    assert_eq!(pane_map.len(), 2);
    assert_eq!(driver.executed_moves.len(), 2);
    assert_eq!(driver.executed_moves[0].0, "w1:p5");
    assert_eq!(driver.executed_moves[1].0, "w1:p6");
}

#[test]
fn test_parse_move_response_and_changed_false() {
    use quaqua::grid::apply::parse_move_response;

    // 1. Success response matching real herdr shape (.result.move_result)
    let ok_json = serde_json::json!({
        "result": {
            "move_result": {
                "changed": true,
                "reason": "ok",
                "pane": {
                    "pane_id": "w1:p10",
                    "tab_id": "w1:t5"
                },
                "previous_pane_id": "w1:p5"
            }
        }
    });
    let res = parse_move_response(&ok_json, "w1:p5").unwrap();
    assert_eq!(res.pane_id, "w1:p10");
    assert_eq!(res.tab_id, "w1:t5");
    assert_eq!(res.previous_pane_id, Some("w1:p5".to_string()));

    // 2. changed=false response (e.g. same_tab or zoomed_tab)
    let failed_json = serde_json::json!({
        "result": {
            "move_result": {
                "changed": false,
                "reason": "same_tab"
            }
        }
    });
    let err = parse_move_response(&failed_json, "w1:p5").unwrap_err();
    assert!(
        err.contains("changed=false, reason=same_tab"),
        "unexpected error message: {err}"
    );
}

#[test]
fn test_deferred_op_propagates_to_dependent_splits() {
    use quaqua::grid::types::Op;
    // When op 0 (new_tab) is deferred, subsequent split ops targeting @workers must also be deferred
    let ops = vec![
        Op {
            op: "move".to_string(),
            pane: "w1:p5".to_string(),
            deferred: true,
            target: Target::NewTab {
                label: "quaqua-workers".to_string(),
            },
        },
        Op {
            op: "move".to_string(),
            pane: "w1:p6".to_string(),
            deferred: false,
            target: Target::Split {
                tab: "@workers".to_string(),
                anchor: "w1:p5".to_string(),
                direction: "right".to_string(),
                ratio: 0.5,
            },
        },
    ];

    let mut driver = FakeHerdrDriver::new(quaqua::grid::Snapshot {
        workspaces: vec![],
        tabs: vec![],
        panes: vec![],
        layouts: vec![],
        agents: vec![],
    });
    let pane_map = apply_ops(&mut driver, &ops, None).unwrap();
    // Neither op should be applied!
    assert_eq!(pane_map.len(), 0);
    assert_eq!(driver.executed_moves.len(), 0);
}

#[test]
fn test_fake_clock_now_sec() {
    use quaqua::grid::reconcile::now_sec;
    let tmp_clock = std::env::temp_dir().join("test_fake_clock.txt");
    fs::write(&tmp_clock, "1729000000\n").unwrap();

    std::env::set_var("PROMPT_WORKER_CLOCK_FILE", &tmp_clock);
    let t = now_sec();
    std::env::remove_var("PROMPT_WORKER_CLOCK_FILE");
    let _ = fs::remove_file(&tmp_clock);

    assert_eq!(t, 1729000000);
}

#[test]
fn test_repair_renames_with_new_pane_ids_and_respects_only() {
    // Finding 1: repair renames moved panes using their new IDs and only renames --only listed workers
    let fix = fixtures_dir().join("2-workers.json");
    let content = fs::read_to_string(fix).unwrap();
    let val: serde_json::Value = serde_json::from_str(&content).unwrap();
    let snap = parse_snapshot_value(&val).unwrap();

    let mut driver = FakeHerdrDriver::new(snap);
    let opts = ReconcileOptions {
        check: false,
        repair: true,
        tab: None,
        only: Some("sude-1".to_string()), // only sude-1 (w1:p6) should be renamed
        owner: Some("cos".to_string()),
        dry_run: false,
        timeout_sec: None,
    };

    run_reconcile(&mut driver, &opts, "workers", None).unwrap();
    // B was moved into quaqua-workers (NewTab), matching bash grid-workers.sh:450-460, :557
    assert_eq!(driver.executed_moves.len(), 1);
    let (moved_pane, target) = &driver.executed_moves[0];
    assert_eq!(moved_pane, "w1:p6");
    match target {
        Target::NewTab { label } => assert_eq!(label, "quaqua-workers"),
        _ => panic!(
            "expected NewTab for B into quaqua-workers, got {:?}",
            target
        ),
    }
    // Only sude-1 should be renamed (tieude-1 was unlisted and must be untouched!)
    assert_eq!(driver.executed_renames.len(), 1);
    let (renamed_pane, label) = &driver.executed_renames[0];
    assert_eq!(label, "sude-1");
    // It must use the NEW pane ID assigned upon move, NOT the stale w1:p6
    assert_ne!(renamed_pane, "w1:p6");
    assert!(renamed_pane.starts_with("w1:p"));
}

#[test]
fn test_apply_ops_rechecks_idle_and_fails_if_busy() {
    // Finding 2: if pane turns busy between snapshot and move, apply_ops must refuse
    let fix = fixtures_dir().join("1-worker.json");
    let content = fs::read_to_string(fix).unwrap();
    let val: serde_json::Value = serde_json::from_str(&content).unwrap();
    let mut snap = parse_snapshot_value(&val).unwrap();

    let ops = plan(&snap).unwrap();
    assert_eq!(ops.len(), 1);

    // Simulate pane becoming busy before move
    if let Some(p) = snap.panes.iter_mut().find(|p| p.pane_id == "w1:p5") {
        p.agent_status = Some("working".to_string());
    }

    let mut driver = FakeHerdrDriver::new(snap);
    let res = apply_ops(&mut driver, &ops, None);
    assert!(res.is_err());
    let err = res.unwrap_err();
    assert!(
        err.contains("became busy; replan required"),
        "unexpected error message: {err}"
    );
}

#[test]
fn test_check_only_waits_when_listed_split_dependency_is_unlisted() {
    // Finding 3: --check --only sude-1 must wait on sude-1's split even if tieude-1 was unlisted
    let fix = fixtures_dir().join("2-workers.json");
    let content = fs::read_to_string(fix).unwrap();
    let val: serde_json::Value = serde_json::from_str(&content).unwrap();
    let snap = parse_snapshot_value(&val).unwrap();

    let mut driver = FakeHerdrDriver::new(snap);
    let opts = ReconcileOptions {
        check: true,
        repair: false,
        tab: Some("quaqua-workers".to_string()),
        only: Some("sude-1".to_string()), // worker 0 (tieude-1) unlisted
        owner: Some("cos".to_string()),
        dry_run: true,
        timeout_sec: Some(0),
    };

    // Must find pending ops and fail with pending ops error, NOT report [] or exit 0!
    let res = run_reconcile(&mut driver, &opts, "workers", None);
    assert!(
        res.is_err(),
        "check must wait on listed worker's split even when worker 0 is unlisted"
    );
    let err = res.unwrap_err();
    assert!(err.contains("pending ops: 1"), "unexpected error: {err}");
}

#[test]
fn test_hook_failure_backoff_uses_post_failure_sig_and_scratch_normalization() {
    // Finding 4: snapshot signature maps grid-scratch- to @scratch and includes non-ready workers
    use quaqua::grid::hook::compute_snapshot_signature;
    let fix = fixtures_dir().join("2-workers.json");
    let content = fs::read_to_string(fix).unwrap();
    let val: serde_json::Value = serde_json::from_str(&content).unwrap();
    let mut snap = parse_snapshot_value(&val).unwrap();

    // Set tab label to a scratch tab
    snap.tabs.push(quaqua::grid::types::Tab {
        tab_id: "w1:t_scratch_1".to_string(),
        workspace_id: Some("w1".to_string()),
        label: Some("grid-scratch-quaqua-999-1".to_string()),
        pane_count: Some(1),
    });
    snap.panes[1].tab_id = Some("w1:t_scratch_1".to_string());

    let sig = compute_snapshot_signature(&snap);
    assert!(
        sig.contains("@scratch"),
        "signature must normalize scratch tab to @scratch: {sig}"
    );
}

#[test]
fn test_apply_ops_verifies_fresh_snapshot_and_resolves_workers_from_anchor() {
    // Finding 5: fresh snapshot verification and @workers resolution from anchor tab
    let fix = fixtures_dir().join("2-workers.json");
    let content = fs::read_to_string(fix).unwrap();
    let val: serde_json::Value = serde_json::from_str(&content).unwrap();
    let mut snap = parse_snapshot_value(&val).unwrap();

    // Mark layouts as zoomed -> apply_ops must refuse with zoomed error
    for l in &mut snap.layouts {
        l.zoomed = Some(true);
    }
    let ops = plan(&snap).unwrap();

    let mut driver = FakeHerdrDriver::new(snap);
    let res = apply_ops(&mut driver, &ops, None);
    assert!(res.is_err());
    let err = res.unwrap_err();
    assert!(err.contains("is zoomed"), "unexpected error message: {err}");
}

#[test]
fn test_hook_locking_and_dead_pid_reclaim() {
    // Finding 6: acquire_lock reclaims dead PID
    use quaqua::grid::hook::acquire_lock;
    let tmp_lock_dir = std::env::temp_dir().join("test_herdr_grid_lock_dir");
    let pid_file = tmp_lock_dir.join("pid");

    let _ = fs::remove_file(&pid_file);
    let _ = fs::remove_dir(&tmp_lock_dir);

    fs::create_dir(&tmp_lock_dir).unwrap();
    // Write a definitely dead PID (e.g. 9999999)
    fs::write(&pid_file, "9999999").unwrap();

    let guard = acquire_lock(&tmp_lock_dir, &pid_file);
    assert!(
        guard.is_ok(),
        "acquire_lock must reclaim lock from dead PID"
    );
    drop(guard);
}

#[test]
fn test_hook_status_early_exit() {
    // Finding 6: should_early_exit checks status transitions
    use quaqua::grid::hook::should_early_exit;
    let fix = fixtures_dir().join("2-workers.json");
    let tmp_snap = std::env::temp_dir().join("test_early_exit_snap.json");
    fs::copy(&fix, &tmp_snap).unwrap();

    // 1. Idle -> done: both non-working -> early exit true
    let event_done = serde_json::json!({
        "pane_id": "w1:p5",
        "agent_status": "done"
    })
    .to_string();
    assert!(should_early_exit(
        Some("pane.agent_status_changed"),
        Some(&event_done),
        &tmp_snap
    ));

    // 2. Idle -> working: state changed -> early exit false
    let event_working = serde_json::json!({
        "pane_id": "w1:p5",
        "agent_status": "working"
    })
    .to_string();
    assert!(!should_early_exit(
        Some("pane.agent_status_changed"),
        Some(&event_working),
        &tmp_snap
    ));

    // 3. Empty status -> do not early exit (Finding 8)
    let event_empty = serde_json::json!({
        "pane_id": "w1:p5"
    })
    .to_string();
    assert!(!should_early_exit(
        Some("pane.agent_status_changed"),
        Some(&event_empty),
        &tmp_snap
    ));

    let _ = fs::remove_file(&tmp_snap);
}

#[test]
fn test_repair_arranges_all_groups_using_tab_as_default() {
    // Finding 7: repair arranges all groups rather than filtering out non-matching groups
    let fix = fixtures_dir().join("cross_checker_owner_group.json");
    let content = fs::read_to_string(fix).unwrap();
    let val: serde_json::Value = serde_json::from_str(&content).unwrap();
    let snap = parse_snapshot_value(&val).unwrap();

    let mut driver = FakeHerdrDriver::new(snap);
    let opts = ReconcileOptions {
        check: false,
        repair: true,
        tab: Some("workers".to_string()), // default tab provided
        only: None,
        owner: Some("lead-1".to_string()),
        dry_run: false,
        timeout_sec: None,
    };

    run_reconcile(&mut driver, &opts, "workers", None).unwrap();
    // Lead has workers in lead-workers group: both moves must be executed!
    assert_eq!(driver.executed_moves.len(), 2);
}

#[test]
fn test_hook_shadow_mode_file_precedence_and_empty_snapshot() {
    // Finding 8: shadow file precedence and empty snapshot
    use quaqua::grid::hook::{is_shadow_mode, reconcile_step};
    // Unique per run: a fixed name shared with a concurrent run, or with
    // whatever an earlier run left behind (a read-only `shadow` file makes
    // every later `fs::write` fail).
    let tmp_state = std::env::temp_dir().join(format!(
        "quaqua-shadow-mode-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    fs::create_dir_all(&tmp_state).unwrap();

    std::env::set_var("HERDR_GRID_SHADOW", "1");
    fs::write(tmp_state.join("shadow"), "0\n").unwrap();
    // Even though env=1, shadow file contains 0 -> live mode (false)
    assert!(!is_shadow_mode(&tmp_state));
    std::env::remove_var("HERDR_GRID_SHADOW");

    // HERDR_QUAQUA_SHADOW takes precedence over the legacy HERDR_GRID_SHADOW
    fs::write(tmp_state.join("shadow"), "1\n").unwrap();
    std::env::set_var("HERDR_GRID_SHADOW", "0");
    std::env::set_var("HERDR_QUAQUA_SHADOW", "1");
    assert!(is_shadow_mode(&tmp_state));
    std::env::remove_var("HERDR_QUAQUA_SHADOW");
    std::env::remove_var("HERDR_GRID_SHADOW");

    // Legacy HERDR_GRID_SHADOW=0 alone (no QUAQUA var, no shadow file) still disables shadow
    let _ = fs::remove_file(tmp_state.join("shadow"));
    std::env::set_var("HERDR_GRID_SHADOW", "0");
    assert!(!is_shadow_mode(&tmp_state));
    std::env::remove_var("HERDR_GRID_SHADOW");

    // Empty snapshot returns Ok(())
    let mut driver = FakeHerdrDriver::new(quaqua::grid::Snapshot {
        workspaces: vec![],
        tabs: vec![],
        panes: vec![],
        layouts: vec![],
        agents: vec![],
    });
    assert!(reconcile_step(&tmp_state, &mut driver, None).is_ok());
    let _ = fs::remove_dir_all(&tmp_state);
}

#[test]
fn test_grid_deferred_statuses_from_config() {
    // Finding 9: plan_with_deferred supports custom deferred_statuses
    use quaqua::grid::plan_with_deferred;
    let fix = fixtures_dir().join("1-worker.json");
    let content = fs::read_to_string(fix).unwrap();
    let val: serde_json::Value = serde_json::from_str(&content).unwrap();
    let mut snap = parse_snapshot_value(&val).unwrap();

    snap.panes[1].agent_status = Some("paused".to_string());

    // With default ["working"], "paused" is not deferred
    let ops_def = plan_with_deferred(&snap, None).unwrap();
    assert!(!ops_def[0].deferred);

    // With custom ["working", "paused"], "paused" is deferred
    let ops_custom = plan_with_deferred(&snap, Some(&["working", "paused"])).unwrap();
    assert!(ops_custom[0].deferred);
}

#[test]
fn test_reviewer_n1_scenario_b_never_lands_in_orchestrator_tab() {
    // Reviewer N1 scenario:
    // Worker A sits beside CoS in w1:t1 (the orchestrator tab!).
    // Worker B spawns in w1:t2.
    // Someone runs `grid reconcile --repair --only B`.
    // B must NEVER land in w1:t1!
    let fix = fixtures_dir().join("2-workers.json");
    let content = fs::read_to_string(fix).unwrap();
    let val: serde_json::Value = serde_json::from_str(&content).unwrap();
    let mut snap = parse_snapshot_value(&val).unwrap();

    // Worker A (w1:p5) sits in w1:t1 beside CoS
    snap.panes[1].tab_id = Some("w1:t1".to_string());

    let mut driver = FakeHerdrDriver::new(snap);
    let opts = ReconcileOptions {
        check: false,
        repair: true,
        tab: None,
        only: Some("sude-1".to_string()), // only B (w1:p6)
        owner: Some("cos".to_string()),
        dry_run: false,
        timeout_sec: None,
    };

    run_reconcile(&mut driver, &opts, "workers", None).unwrap();

    // B must be moved
    assert_eq!(driver.executed_moves.len(), 1);
    let (moved_pane, target) = &driver.executed_moves[0];
    assert_eq!(moved_pane, "w1:p6");

    // Critical assertion: B must NEVER target w1:t1 (orchestrator tab)!
    match target {
        Target::NewTab { label } => {
            assert_eq!(label, "quaqua-workers");
        }
        Target::Split { tab, .. } => {
            assert_ne!(
                tab, "w1:t1",
                "worker B must NEVER land in orchestrator tab w1:t1"
            );
        }
    }
}

#[test]
fn test_apply_ops_refuses_when_anchor_not_in_target_tab() {
    // Finding 5 remainder: anchor must be in target tab (apply.sh:209-213)
    use quaqua::grid::types::Op;
    let fix = fixtures_dir().join("2-workers.json");
    let content = fs::read_to_string(fix).unwrap();
    let val: serde_json::Value = serde_json::from_str(&content).unwrap();
    let snap = parse_snapshot_value(&val).unwrap();

    // Op targets w1:t2, but anchor w1:p1 is in w1:t1!
    let ops = vec![Op {
        op: "move".to_string(),
        pane: "w1:p5".to_string(),
        deferred: false,
        target: Target::Split {
            tab: "w1:t2".to_string(),
            anchor: "w1:p1".to_string(), // w1:p1 is in w1:t1!
            direction: "right".to_string(),
            ratio: 0.5,
        },
    }];

    let mut driver = FakeHerdrDriver::new(snap);
    let res = apply_ops(&mut driver, &ops, None);
    assert!(res.is_err());
    let err = res.unwrap_err();
    assert!(
        err.contains("expected target tab w1:t2"),
        "unexpected error message: {err}"
    );
}

struct FailingRenameDriver {
    fake: FakeHerdrDriver,
}

impl quaqua::grid::HerdrDriver for FailingRenameDriver {
    fn snapshot(&mut self) -> Result<quaqua::grid::Snapshot, String> {
        self.fake.snapshot()
    }
    fn move_pane(
        &mut self,
        pane: &str,
        target: &Target,
    ) -> Result<quaqua::grid::MoveResult, String> {
        self.fake.move_pane(pane, target)
    }
    fn rename_pane(&mut self, _pane: &str, _label: &str) -> Result<(), String> {
        Err("pane rename failed: connection reset".to_string())
    }
}

#[test]
fn test_repair_fails_if_rename_fails() {
    // Finding 1 remainder: failed rename must not be swallowed (matches bash set -e)
    let fix = fixtures_dir().join("1-worker.json");
    let content = fs::read_to_string(fix).unwrap();
    let val: serde_json::Value = serde_json::from_str(&content).unwrap();
    let snap = parse_snapshot_value(&val).unwrap();

    let mut driver = FailingRenameDriver {
        fake: FakeHerdrDriver::new(snap),
    };
    let opts = ReconcileOptions {
        check: false,
        repair: true,
        tab: None,
        only: Some("tieude-1".to_string()),
        owner: Some("cos".to_string()),
        dry_run: false,
        timeout_sec: None,
    };

    let res = run_reconcile(&mut driver, &opts, "workers", None);
    assert!(res.is_err(), "rename failure must be propagated");
    let err = res.unwrap_err();
    assert!(
        err.contains("pane rename failed"),
        "unexpected error: {err}"
    );
}

#[test]
fn test_reconcile_honours_deferred_statuses() {
    // Finding 9 remainder: reconcile check honours custom deferred_statuses
    let fix = fixtures_dir().join("1-worker.json");
    let content = fs::read_to_string(fix).unwrap();
    let val: serde_json::Value = serde_json::from_str(&content).unwrap();
    let mut snap = parse_snapshot_value(&val).unwrap();

    // Mark worker as paused
    snap.panes[1].agent_status = Some("paused".to_string());

    let mut driver = FakeHerdrDriver::new(snap);
    let opts = ReconcileOptions {
        check: true,
        repair: false,
        tab: Some("workers".to_string()),
        only: None,
        owner: Some("cos".to_string()),
        dry_run: true,
        timeout_sec: Some(0),
    };

    // When "paused" is in deferred_statuses, check converges immediately with 0 pending ops!
    let res = run_reconcile(&mut driver, &opts, "workers", Some(&["working", "paused"]));
    assert!(
        res.is_ok(),
        "paused worker must be deferred and converge immediately"
    );
}

fn load_edge_fixture(name: &str) -> quaqua::grid::Snapshot {
    let fix = fixtures_dir().join(format!("{name}.json"));
    let content = fs::read_to_string(&fix)
        .unwrap_or_else(|e| panic!("failed reading {}: {e}", fix.display()));
    let val: serde_json::Value = serde_json::from_str(&content).unwrap();
    parse_snapshot_value(&val).unwrap()
}

#[test]
fn test_edge_fixture_worker_in_wrong_tab() {
    let snap = load_edge_fixture("worker_in_wrong_tab");
    let ops = plan(&snap).expect("plan must succeed for worker_in_wrong_tab");
    assert_eq!(ops.len(), 2);
    assert_eq!(ops[0].pane, "w1:p5");
    assert_eq!(ops[1].pane, "w1:p6");
    assert!(matches!(&ops[0].target, Target::NewTab { label } if label == "quaqua-workers"));
    assert!(
        matches!(&ops[1].target, Target::Split { tab, anchor, .. } if tab == "@workers" && anchor == "w1:p5")
    );
}

#[test]
fn test_edge_fixture_worker_with_foreign_pane() {
    let snap = load_edge_fixture("worker_with_foreign_pane");
    let ops = plan(&snap).expect("plan must succeed for worker_with_foreign_pane");
    assert_eq!(ops.len(), 2);
    assert_eq!(ops[0].pane, "w1:p5");
    assert_eq!(ops[1].pane, "w1:p6");
    assert!(matches!(&ops[0].target, Target::NewTab { label } if label == "quaqua-workers"));
}

#[test]
fn test_edge_fixture_zoomed_anchor_tab() {
    // edgecases §4: "validate tab zoom state before first pane move". The
    // planner still targets the anchor tab, and apply_ops must refuse it while
    // that tab is zoomed — the fixture's whole point.
    let snap = load_edge_fixture("zoomed_anchor_tab");
    assert!(
        snap.layouts.iter().any(|l| l.zoomed == Some(true)),
        "fixture must carry the zoom state"
    );
    let ops = plan(&snap).expect("plan must succeed for zoomed_anchor_tab");
    assert_eq!(ops.len(), 1);
    assert_eq!(ops[0].pane, "w1:p5");
    assert!(
        matches!(&ops[0].target, Target::Split { tab, anchor, .. } if tab == "w1:t1" && anchor == "w1:p1")
    );

    let mut driver = FakeHerdrDriver::new(snap);
    let err =
        apply_ops(&mut driver, &ops, None).expect_err("apply must refuse a zoomed anchor tab");
    assert!(err.contains("is zoomed"), "unexpected refusal: {err}");
    assert!(
        driver.executed_moves.is_empty(),
        "no pane may move while the anchor tab is zoomed: {:?}",
        driver.executed_moves
    );
}

#[test]
fn test_edge_fixture_missing_workers_tab() {
    let snap = load_edge_fixture("missing_workers_tab");
    let ops = plan(&snap).expect("plan must succeed for missing_workers_tab");
    assert_eq!(ops.len(), 2);
    assert_eq!(ops[0].pane, "w1:p5");
    assert!(matches!(&ops[0].target, Target::NewTab { label } if label == "quaqua-workers"));
    assert!(
        matches!(&ops[1].target, Target::Split { tab, anchor, .. } if tab == "@workers" && anchor == "w1:p5")
    );
}

#[test]
fn test_edge_fixture_stale_session_path() {
    let snap = load_edge_fixture("stale_session_path");
    let ops = plan(&snap).expect("plan must succeed for stale_session_path");
    assert_eq!(ops.len(), 1);
    assert_eq!(ops[0].pane, "w1:p5");
    assert!(
        matches!(&ops[0].target, Target::Split { tab, anchor, .. } if tab == "w1:t1" && anchor == "w1:p1")
    );
}

/// A driver that fails every move while `fail` is set, then delegates to the
/// fake driver — the fixture files cannot describe a failure, so the backoff
/// test has to cause one.
struct FailOnceDriver {
    inner: FakeHerdrDriver,
    fail: bool,
}

impl HerdrDriver for FailOnceDriver {
    fn snapshot(&mut self) -> Result<Snapshot, String> {
        self.inner.snapshot()
    }
    fn move_pane(&mut self, pane: &str, target: &Target) -> Result<MoveResult, String> {
        if self.fail {
            return Err("simulated apply failure".to_string());
        }
        self.inner.move_pane(pane, target)
    }
    fn rename_pane(&mut self, pane: &str, label: &str) -> Result<(), String> {
        self.inner.rename_pane(pane, label)
    }
}

fn edge_state_dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("quaqua-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&d);
    fs::create_dir_all(&d).unwrap();
    fs::write(d.join("shadow"), "0").unwrap(); // live mode: reconcile applies ops
    d
}

#[test]
fn test_edge_fixture_event_storm_after_move() {
    // The layout has already converged. A storm of post-move events must be
    // absorbed: every pass on the unchanged snapshot is a no-op, no pane moves
    // a second time, and the debounce key (the snapshot signature) is stable.
    let snap = load_edge_fixture("event_storm_after_move");
    let ops = plan(&snap).expect("plan must succeed for event_storm_after_move");
    assert!(
        ops.is_empty(),
        "event storm after move should converge with 0 ops"
    );

    let sdir = edge_state_dir("edge-storm");
    let mut driver = FakeHerdrDriver::new(snap.clone());
    for pass in 0..5 {
        reconcile_step(&sdir, &mut driver, None)
            .unwrap_or_else(|e| panic!("storm pass {pass} must be a no-op: {e}"));
    }
    assert!(
        driver.executed_moves.is_empty(),
        "an event storm on a converged layout must not move anything: {:?}",
        driver.executed_moves
    );
    let log = fs::read_to_string(sdir.join("shadow.log")).unwrap_or_default();
    assert!(
        !log.contains("apply ops:"),
        "storm must never reach apply: {log}"
    );
    let cached: Snapshot =
        serde_json::from_str(&fs::read_to_string(sdir.join("last_snapshot.json")).unwrap())
            .unwrap();
    assert_eq!(
        compute_snapshot_signature(&snap),
        compute_snapshot_signature(&cached),
        "the cached snapshot (the debounce key) must not churn across the storm"
    );
    let _ = fs::remove_dir_all(&sdir);
}

#[test]
fn test_edge_fixture_divide_initial_empty_workspace() {
    let snap = load_edge_fixture("divide_initial_empty_workspace");
    let ops = plan(&snap).expect("plan must succeed for divide_initial_empty_workspace");
    assert!(ops.is_empty(), "empty workspace should yield 0 ops");
}

#[test]
fn test_edge_fixture_failed_apply_then_backoff() {
    // One split is planned (the worker sits in scratch, the workers tab holds
    // the anchor). Live reconcile must try it, record the backoff state when it
    // fails, then skip that op on the unchanged layout instead of looping.
    let snap = load_edge_fixture("failed_apply_then_backoff");
    let ops = plan(&snap).expect("plan must succeed for failed_apply_then_backoff");
    assert_eq!(ops.len(), 1);
    assert_eq!(ops[0].pane, "w1:p6");
    assert!(
        matches!(&ops[0].target, Target::Split { tab, anchor, .. } if tab == "w1:t2" && anchor == "w1:p5")
    );

    let sdir = edge_state_dir("edge-backoff");
    let mut driver = FailOnceDriver {
        inner: FakeHerdrDriver::new(snap),
        fail: true,
    };
    let err = reconcile_step(&sdir, &mut driver, None)
        .expect_err("a failed apply must surface as an error");
    assert!(
        err.contains("simulated apply failure"),
        "unexpected error: {err}"
    );

    let failed_ops = fs::read_to_string(sdir.join("failed_ops.json"))
        .expect("backoff must record the failed ops");
    assert!(failed_ops.contains("w1:p6"), "failed ops: {failed_ops}");
    let failed_sig = fs::read_to_string(sdir.join("failed_snap.json"))
        .expect("backoff must record the snapshot signature");
    assert!(
        !failed_sig.trim().is_empty(),
        "backoff signature must not be empty"
    );

    // Same (unchanged) layout again: the previously failed op is skipped.
    driver.fail = false;
    reconcile_step(&sdir, &mut driver, None).expect("the backoff pass must not error");
    assert!(
        driver.inner.executed_moves.is_empty(),
        "backoff must skip the previously failed op: {:?}",
        driver.inner.executed_moves
    );
    let log = fs::read_to_string(sdir.join("shadow.log")).unwrap_or_default();
    assert!(
        log.contains("backoff: skipping previously failed ops"),
        "no backoff log: {log}"
    );
    let _ = fs::remove_dir_all(&sdir);
}

#[test]
fn test_edge_fixture_cross_checker_owner_group() {
    let snap = load_edge_fixture("cross_checker_owner_group");
    let ops = plan(&snap).expect("plan must succeed for cross_checker_owner_group");
    assert_eq!(ops.len(), 3);
    assert_eq!(ops[0].pane, "w1:p5");
    assert_eq!(ops[1].pane, "w1:p6");
    assert_eq!(ops[2].pane, "w1:p7");
    assert!(
        matches!(&ops[0].target, Target::Split { tab, anchor, .. } if tab == "w1:t1" && anchor == "w1:p1")
    );
    assert!(matches!(&ops[1].target, Target::NewTab { label } if label == "lead-workers"));
    assert!(
        matches!(&ops[2].target, Target::Split { tab, anchor, .. } if tab == "@workers" && anchor == "w1:p6")
    );
}

#[test]
fn test_reconcile_converges_when_single_worker_already_beside_anchor() {
    // Regression: the check used to demand the 2/5|3/5 rects, but a herdr pane
    // split lands 50/50 and the repair path accepts "same tab as the anchor".
    // That disagreement made `--check` time out on an already-arranged pane, so
    // `worker prompt` was blocked right after a spawn.
    let snap = load_edge_fixture("single-worker-in-anchor-tab");
    // The fixture is a real capture: 206x78 split at 103|103, not the 82|124 the
    // old rect test wanted.
    let l = snap.layouts.iter().find(|l| l.tab_id == "w9:t1A").unwrap();
    assert_eq!(
        l.panes[0].rect.unwrap().width,
        l.panes[1].rect.unwrap().width
    );

    let ops = plan(&snap).expect("plan must succeed");
    assert!(
        ops.is_empty(),
        "a worker beside its anchor needs no move: {ops:?}"
    );

    let mut driver = FakeHerdrDriver::new(snap);
    let opts = ReconcileOptions {
        check: true,
        repair: false,
        tab: Some("quaqua-workers".to_string()),
        only: Some("tieude-1".to_string()),
        owner: Some("cos".to_string()),
        dry_run: true,
        timeout_sec: Some(0),
    };
    assert!(
        run_reconcile(&mut driver, &opts, "workers", None).is_ok(),
        "check must converge on an already-arranged single worker"
    );
}

#[test]
fn test_check_converges_when_dependent_split_follows_deferred_anchor() {
    // Regression: a split whose anchor is working is unrunnable (apply defers it),
    // but the plan used to flag it non-deferred, so `--check` counted it as
    // pending and timed out until the anchor idled.
    let snap = load_edge_fixture("deferred-anchor-dependent-split");
    let ops = plan(&snap).expect("plan must succeed");
    assert_eq!(ops.len(), 2);
    assert!(
        ops.iter().all(|op| op.deferred),
        "all ops depend on a working anchor: {ops:?}"
    );

    let mut driver = FakeHerdrDriver::new(snap);
    let opts = ReconcileOptions {
        check: true,
        repair: false,
        tab: Some("quaqua-workers".to_string()),
        only: Some("sude-1".to_string()),
        owner: Some("cos".to_string()),
        dry_run: true,
        timeout_sec: Some(0),
    };
    assert!(
        run_reconcile(&mut driver, &opts, "workers", None).is_ok(),
        "check must converge while the anchor is still working"
    );
}

#[test]
fn test_repair_pivots_new_tab_on_idle_worker_and_defers_working() {
    // The group's first op used to be the new_tab, owned by the working pane:
    // apply_ops deferred it and propagated the deferral to the whole group, so
    // `grid reconcile --owner cos --tab quaqua-rs-workers` exited 0 having moved
    // nothing (and created a tab labelled with the group token, not --tab).
    // The repair must pivot the tab on the idle worker and report the working one.
    let snap = load_edge_fixture("deferred-anchor-dependent-split");
    let mut driver = FakeHerdrDriver::new(snap);
    let opts = repair_opts("cos", "quaqua-rs-workers");

    run_reconcile(&mut driver, &opts, "workers", None).unwrap();
    assert_eq!(
        driver.executed_moves.len(),
        1,
        "one runnable move: {:?}",
        driver.executed_moves
    );
    let (moved, target) = &driver.executed_moves[0];
    assert_eq!(moved, "w1:p6", "the idle worker creates the tab");
    match target {
        Target::NewTab { label } => assert_eq!(label, "quaqua-rs-workers"),
        other => panic!("expected NewTab labelled with --tab, got {other:?}"),
    }
    assert!(
        driver
            .current_snapshot
            .panes
            .iter()
            .any(|p| p.pane_id == "w1:p5" && p.agent_status.as_deref() == Some("working")),
        "the working worker must not move"
    );

    // Once the working worker idles, a second repair pass completes the group.
    idle_the_working_panes(&mut driver);
    driver.executed_moves.clear();
    run_reconcile(&mut driver, &opts, "workers", None).unwrap();
    assert_eq!(
        driver.executed_moves.len(),
        1,
        "second pass moves the now-idle worker"
    );
    let (moved2, target2) = &driver.executed_moves[0];
    assert_eq!(moved2, "w1:p5");
    let workers_tab = tab_labelled(&driver, "quaqua-rs-workers").expect("tab labelled --tab");
    match target2 {
        Target::Split { tab, anchor, .. } => {
            assert_eq!(tab, &workers_tab, "the second pass reuses the tab it named");
            assert_eq!(anchor, &pane_id_by_label(&driver, "sude-1"));
        }
        other => panic!("expected a split into the workers tab, got {other:?}"),
    }

    // Converged: nothing left to plan or move.
    driver.executed_moves.clear();
    run_reconcile(&mut driver, &opts, "workers", None).unwrap();
    assert!(
        driver.executed_moves.is_empty(),
        "converged repair: {:?}",
        driver.executed_moves
    );
}

#[test]
fn test_two_pipelines_same_owner_land_in_grid_tab() {
    // Two pipelines of one owner (a Lead) spawn builders into the Lead's tab.
    // The repair must put the owner's workers in the tab labelled GRID_TAB and
    // never in the Lead's tab: the idle builder goes now, the working one is
    // reported deferred and follows as soon as it idles.
    let snap = load_edge_fixture("two-pipelines-lead-tab");
    let mut driver = FakeHerdrDriver::new(snap);
    let opts = repair_opts("quaqua-rs-lead", "quaqua-rs-workers");

    run_reconcile(&mut driver, &opts, "workers", None).unwrap();
    assert_eq!(
        driver.executed_moves.len(),
        1,
        "only the idle builder moves: {:?}",
        driver.executed_moves
    );
    let (moved, target) = &driver.executed_moves[0];
    assert_eq!(moved, "w1:p6");
    match target {
        Target::NewTab { label } => assert_eq!(label, "quaqua-rs-workers"),
        other => panic!("expected NewTab labelled GRID_TAB, got {other:?}"),
    }
    for (_, t) in &driver.executed_moves {
        match t {
            Target::Split { tab, .. } => assert_ne!(tab, "w1:t1", "never into the Lead's tab"),
            Target::NewTab { label } => assert_ne!(label, "lead-tab"),
        }
    }
    assert_eq!(
        panes_in(&driver, "w1:t1").len(),
        2,
        "lead + the working builder so far"
    );

    // The working builder B idles (its prompt finished) -> the next pass moves it.
    idle_the_working_panes(&mut driver);
    driver.executed_moves.clear();
    run_reconcile(&mut driver, &opts, "workers", None).unwrap();
    assert_eq!(
        driver.executed_moves.len(),
        1,
        "second pass moves the now-idle builder"
    );
    assert_eq!(driver.executed_moves[0].0, "w1:p5");
    match &driver.executed_moves[0].1 {
        Target::Split { tab, anchor, .. } => {
            assert_ne!(tab, "w1:t1", "never into the Lead's tab");
            assert_eq!(anchor, &pane_id_by_label(&driver, "sude-pagetwo"));
        }
        other => panic!("expected a split into the workers tab, got {other:?}"),
    }

    // Both builders in the tab labelled GRID_TAB; the Lead alone in its own tab.
    let workers_tab = tab_labelled(&driver, "quaqua-rs-workers").expect("tab labelled GRID_TAB");
    let workers_in_tab = panes_in(&driver, &workers_tab);
    assert_eq!(
        workers_in_tab.len(),
        2,
        "both builders in GRID_TAB: {workers_in_tab:?}"
    );
    assert_eq!(
        panes_in(&driver, "w1:t1"),
        vec!["w1:p1".to_string()],
        "Lead stays alone"
    );

    driver.executed_moves.clear();
    run_reconcile(&mut driver, &opts, "workers", None).unwrap();
    assert!(
        driver.executed_moves.is_empty(),
        "converged repair: {:?}",
        driver.executed_moves
    );
}

#[test]
fn test_repair_keeps_idle_panes_movable_when_the_first_worker_is_deferred() {
    // n>=3 with the first worker working: the pivot creates the tab, and the
    // remaining *idle* workers must still move — they used to anchor on the
    // working worker, so propagate_deferrals cancelled them as well.
    let mut snap = load_edge_fixture("3-workers");
    for p in &mut snap.panes {
        if p.pane_id == "w1:p5" {
            p.agent_status = Some("working".to_string());
        }
    }
    let mut driver = FakeHerdrDriver::new(snap);
    let opts = repair_opts("cos", "quaqua-rs-workers");

    run_reconcile(&mut driver, &opts, "workers", None).unwrap();
    let moved: Vec<&str> = driver
        .executed_moves
        .iter()
        .map(|(p, _)| p.as_str())
        .collect();
    assert_eq!(
        moved,
        vec!["w1:p6", "w1:p7"],
        "both idle panes move: {:?}",
        driver.executed_moves
    );
    assert!(
        matches!(&driver.executed_moves[0].1, Target::NewTab { label } if label == "quaqua-rs-workers")
    );
    match &driver.executed_moves[1].1 {
        Target::Split { anchor, .. } => {
            assert_eq!(
                anchor,
                &pane_id_by_label(&driver, "sude-1"),
                "anchored on the idle pivot"
            );
        }
        other => panic!("expected a split into the workers tab, got {other:?}"),
    }
    assert!(
        driver
            .current_snapshot
            .panes
            .iter()
            .any(|p| p.pane_id == "w1:p5" && p.agent_status.as_deref() == Some("working")),
        "the working pane stays where it is"
    );
}

#[test]
fn test_plan_accepts_the_tab_the_repair_made() {
    // The reviewer's rule: a converged-check and the repair it guards must share
    // one acceptance predicate. The repair labels the tab with `--tab`
    // (`quaqua-rs-workers`) while the workers' group token is `quaqua`, so the
    // planner used to plan a *new* tab and the hook moved the panes back out.
    for fixture in ["deferred-anchor-dependent-split", "two-pipelines-lead-tab"] {
        let mut driver = FakeHerdrDriver::new(load_edge_fixture(fixture));
        let opts = repair_opts(
            if fixture == "two-pipelines-lead-tab" {
                "quaqua-rs-lead"
            } else {
                "cos"
            },
            "quaqua-rs-workers",
        );

        // converge: one pass moves the idle worker, a second the working one
        run_reconcile(&mut driver, &opts, "workers", None).unwrap();
        idle_the_working_panes(&mut driver);
        run_reconcile(&mut driver, &opts, "workers", None).unwrap();

        let ops = plan(&driver.current_snapshot).unwrap();
        assert!(
            ops.is_empty(),
            "{fixture}: the planner must accept the repair's tab, got {ops:?}"
        );
        assert!(
            tab_labelled(&driver, "quaqua-rs-workers").is_some(),
            "{fixture}: the workers tab is labelled --tab"
        );
    }
}

#[test]
fn test_plan_and_repair_agree_on_a_foreign_workers_tab() {
    // Finding 1: a foreign `<X>-workers` tab (`mono-workers`) holds only this
    // group's panes while the group's own spawn-labelled tab
    // (`quaqua-rs-workers`) holds the rest. The planner used to widen on
    // `ends_with("-workers")` and adopt the foreign tab (first in snapshot
    // order) while the repair only accepted its own `--tab` label: hook and
    // repair pulled the panes in opposite directions. Both now share
    // `accepts_workers_tab`, so both choose the tab the panes were spawned into.
    let snap = load_edge_fixture("foreign-workers-tab");
    let ops = plan(&snap).expect("plan must succeed");
    assert_eq!(
        ops.len(),
        1,
        "only the pane outside the spawn tab moves: {ops:?}"
    );
    assert_eq!(ops[0].pane, "w1:p5");
    match &ops[0].target {
        Target::Split { tab, .. } => {
            assert_eq!(
                tab, "w1:t3",
                "planner must target the spawn-labelled tab, never mono-workers"
            );
        }
        other => panic!("expected a split into the workers tab, got {other:?}"),
    }

    let mut driver = FakeHerdrDriver::new(snap);
    let opts = repair_opts("cos", "quaqua-rs-workers");
    run_reconcile(&mut driver, &opts, "workers", None).unwrap();
    assert_eq!(
        driver.executed_moves.len(),
        1,
        "repair moves only w1:p5: {:?}",
        driver.executed_moves
    );
    let (moved, target) = &driver.executed_moves[0];
    assert_eq!(moved, "w1:p5");
    match target {
        Target::Split { tab, .. } => {
            assert_eq!(
                tab, "w1:t3",
                "repair must choose the same tab as the planner"
            );
        }
        other => panic!("expected a split into the workers tab, got {other:?}"),
    }
    assert!(
        panes_in(&driver, "w1:t2").is_empty(),
        "the foreign mono-workers tab must never be adopted: {:?}",
        panes_in(&driver, "w1:t2")
    );
}

#[test]
fn test_check_is_not_vacuous_when_grid_tab_differs_from_group() {
    // Finding 2: with GRID_TAB (`quaqua-rs-workers`) != the group token
    // (`quaqua`), the check dropped every op (`is_group_tab` false) and returned
    // Ok on any layout. The shared predicate accepts the pane's spawn label, so
    // an unarranged layout must report pending ops.
    let snap = load_edge_fixture("grid-tab-unarranged");
    let ops = plan(&snap).expect("plan must succeed");
    assert_eq!(ops.len(), 2);
    assert!(
        matches!(&ops[0].target, Target::NewTab { label } if label == "quaqua-rs-workers"),
        "the new tab must carry the spawn label, got {:?}",
        ops[0].target
    );

    let mut driver = FakeHerdrDriver::new(snap);
    let opts = ReconcileOptions {
        check: true,
        repair: false,
        tab: Some("quaqua-rs-workers".to_string()),
        only: None,
        owner: Some("cos".to_string()),
        dry_run: true,
        timeout_sec: Some(0),
    };
    let res = run_reconcile(&mut driver, &opts, "workers", None);
    assert!(
        res.is_err(),
        "check must not be vacuous when GRID_TAB != group"
    );
    assert!(
        res.unwrap_err().contains("pending ops"),
        "the check must report the pending ops"
    );
}

#[test]
fn test_check_converges_after_repair_when_grid_tab_differs_from_group() {
    // The same predicate that finds the pending ops must accept the tab the
    // repair makes, or the pre-prompt check never converges on that layout.
    let snap = load_edge_fixture("grid-tab-unarranged");
    let mut driver = FakeHerdrDriver::new(snap);
    let opts = repair_opts("cos", "quaqua-rs-workers");
    run_reconcile(&mut driver, &opts, "workers", None).unwrap();

    let check = ReconcileOptions {
        check: true,
        repair: false,
        tab: Some("quaqua-rs-workers".to_string()),
        only: None,
        owner: Some("cos".to_string()),
        dry_run: true,
        timeout_sec: Some(0),
    };
    assert!(
        run_reconcile(&mut driver, &check, "workers", None).is_ok(),
        "check must converge on the repaired layout"
    );
}

#[test]
fn test_repair_anchors_on_working_pane_in_workers_tab_without_self_anchor() {
    // Defect 3: when the only pane in the workers tab is busy (working),
    // an unarranged worker being moved into the workers tab must anchor on
    // that pane (w1:p5) and NEVER anchor on itself (w1:p6).
    let fix = fixtures_dir().join("busy-worker-in-tab.json");
    let content = fs::read_to_string(fix).unwrap();
    let val: serde_json::Value = serde_json::from_str(&content).unwrap();
    let snap = parse_snapshot_value(&val).unwrap();

    let mut driver = FakeHerdrDriver::new(snap);
    let opts = repair_opts("quaqua-rs-lead", "quaqua-workers");

    let res = run_reconcile(&mut driver, &opts, "workers", None);
    assert!(res.is_ok(), "repair must succeed: {:?}", res);
    assert_eq!(driver.executed_moves.len(), 1);
    let (pane, target) = &driver.executed_moves[0];
    assert_eq!(pane, "w1:p6");
    match target {
        Target::Split { tab, anchor, .. } => {
            assert_eq!(tab, "w1:t2");
            assert_eq!(
                anchor, "w1:p5",
                "anchor must be the pane in tab, not itself"
            );
        }
        _ => panic!("expected split target, got: {:?}", target),
    }
}

#[test]
fn test_reconcile_and_check_converge_for_overflow_workers() {
    let fix = fixtures_dir().join("5-workers.json");
    let content = fs::read_to_string(fix).unwrap();
    let val: serde_json::Value = serde_json::from_str(&content).unwrap();
    let snap = parse_snapshot_value(&val).unwrap();

    let mut driver = FakeHerdrDriver::new(snap);
    let opts = repair_opts("cos", "quaqua-workers");

    // Repair creates quaqua-workers (4 panes) and quaqua-workers-2 (1 pane)
    let res = run_reconcile(&mut driver, &opts, "quaqua-workers", None);
    assert!(res.is_ok(), "repair must succeed: {:?}", res);
    assert_eq!(driver.executed_moves.len(), 5);

    // Check immediately converges with 0 pending ops
    let mut check_opts = opts.clone();
    check_opts.repair = false;
    check_opts.check = true;
    let check_res = run_reconcile(&mut driver, &check_opts, "quaqua-workers", None);
    assert!(check_res.is_ok(), "check must converge: {:?}", check_res);
}

#[test]
fn test_shrink_across_chunks_does_not_duplicate_overflow_tab() {
    // Finding 2 repro: 6 workers originally laid out as
    // quaqua-workers = [p6, p7, p8, p9], quaqua-workers-2 = [pA, pB] (after p5 cleanup: 5 workers).
    // Chunk 0 is [p6, p7, p8, p9], Chunk 1 is [pA].
    // Tab quaqua-workers-2 (holding p9 and pA) must be recognized as valid
    // for Chunk 1 without emitting a duplicate NewTab for quaqua-workers-2.
    let val = serde_json::json!({
        "snapshot": {
            "workspaces": [{ "workspace_id": "w1", "label": "quaqua" }],
            "tabs": [
                { "tab_id": "w1:t1", "workspace_id": "w1", "label": "main", "pane_count": 1 },
                { "tab_id": "w1:t2", "workspace_id": "w1", "label": "quaqua-workers", "pane_count": 3 },
                { "tab_id": "w1:t3", "workspace_id": "w1", "label": "quaqua-workers-2", "pane_count": 2 }
            ],
            "panes": [
                { "pane_id": "w1:p1", "tab_id": "w1:t1", "workspace_id": "w1", "agent_status": "idle", "tokens": { "grid_role": "orchestrator", "grid_owner": "cos", "grid_ready": "1" } },
                { "pane_id": "w1:p6", "tab_id": "w1:t2", "workspace_id": "w1", "agent_status": "idle", "tokens": { "grid_role": "worker", "grid_owner": "cos", "grid_group": "quaqua-workers", "grid_ready": "1" } },
                { "pane_id": "w1:p7", "tab_id": "w1:t2", "workspace_id": "w1", "agent_status": "idle", "tokens": { "grid_role": "worker", "grid_owner": "cos", "grid_group": "quaqua-workers", "grid_ready": "1" } },
                { "pane_id": "w1:p8", "tab_id": "w1:t2", "workspace_id": "w1", "agent_status": "idle", "tokens": { "grid_role": "worker", "grid_owner": "cos", "grid_group": "quaqua-workers", "grid_ready": "1" } },
                { "pane_id": "w1:p9", "tab_id": "w1:t3", "workspace_id": "w1", "agent_status": "idle", "tokens": { "grid_role": "worker", "grid_owner": "cos", "grid_group": "quaqua-workers", "grid_ready": "1" } },
                { "pane_id": "w1:pA", "tab_id": "w1:t3", "workspace_id": "w1", "agent_status": "idle", "tokens": { "grid_role": "worker", "grid_owner": "cos", "grid_group": "quaqua-workers", "grid_ready": "1" } }
            ],
            "layouts": [
                {
                    "tab_id": "w1:t2", "workspace_id": "w1", "area": { "x": 0, "y": 0, "width": 200, "height": 50 },
                    "panes": [
                        { "pane_id": "w1:p6", "rect": { "x": 0, "y": 0, "width": 100, "height": 25 } },
                        { "pane_id": "w1:p7", "rect": { "x": 100, "y": 0, "width": 100, "height": 25 } },
                        { "pane_id": "w1:p8", "rect": { "x": 0, "y": 25, "width": 200, "height": 25 } }
                    ]
                },
                {
                    "tab_id": "w1:t3", "workspace_id": "w1", "area": { "x": 0, "y": 0, "width": 200, "height": 50 },
                    "panes": [
                        { "pane_id": "w1:p9", "rect": { "x": 0, "y": 0, "width": 100, "height": 50 } },
                        { "pane_id": "w1:pA", "rect": { "x": 100, "y": 0, "width": 100, "height": 50 } }
                    ]
                }
            ],
            "agents": []
        }
    });

    let snap = parse_snapshot_value(&val).unwrap();
    let ops = plan(&snap).unwrap();
    let new_tab_ops: Vec<&quaqua::grid::Op> = ops
        .iter()
        .filter(|op| matches!(op.target, Target::NewTab { .. }))
        .collect();
    assert!(
        new_tab_ops.is_empty(),
        "must not create a new tab when quaqua-workers-2 exists: {:?}",
        new_tab_ops
    );
    assert_eq!(ops.len(), 1);
    assert_eq!(ops[0].pane, "w1:p9");
    match &ops[0].target {
        Target::Split { tab, .. } => assert_eq!(tab, "w1:t2"),
        _ => panic!("expected split into w1:t2, got: {:?}", ops[0].target),
    }
}

#[test]
fn test_hook_and_apply_deferred_chunk_2() {
    // Finding 5: test deferral in Chunk 1 NewTab
    let fix = fixtures_dir().join("5-workers.json");
    let content = fs::read_to_string(fix).unwrap();
    let mut val: serde_json::Value = serde_json::from_str(&content).unwrap();
    // Mark w1:p9 (the Chunk 1 creator) as working
    val["snapshot"]["panes"][5]["agent_status"] = serde_json::Value::String("working".to_string());
    let snap = parse_snapshot_value(&val).unwrap();

    let ops = plan(&snap).unwrap();
    assert_eq!(ops.len(), 5);
    // Panes 0..3 are runnable (not deferred)
    assert!(!ops[0].deferred);
    assert!(!ops[1].deferred);
    assert!(!ops[2].deferred);
    assert!(!ops[3].deferred);
    // Pane 4 (w1:p9) is deferred
    assert!(ops[4].deferred);

    // apply_ops applies runnable ops (Chunk 0) and skips deferred Chunk 1
    let mut driver = FakeHerdrDriver::new(snap);
    let res = quaqua::grid::apply::apply_ops(&mut driver, &ops, None);
    assert!(res.is_ok(), "apply_ops must succeed: {:?}", res);
    assert_eq!(
        driver.executed_moves.len(),
        4,
        "must execute only the 4 runnable ops of chunk 0"
    );
}

#[test]
fn test_apply_ops_multiple_groups_each_create_tab() {
    // Round 2 Finding 1 regression repro:
    // When 2+ groups create tabs in one apply pass, @workers must resolve
    // to each group's own tab, never to another group's tab.
    let val = serde_json::json!({
        "snapshot": {
            "workspaces": [{ "workspace_id": "w1", "label": "main" }],
            "tabs": [
                { "tab_id": "w1:t1", "workspace_id": "w1", "label": "main", "pane_count": 1 }
            ],
            "panes": [
                { "pane_id": "w1:p1", "tab_id": "w1:t1", "workspace_id": "w1", "agent_status": "idle", "tokens": { "grid_role": "orchestrator", "grid_owner": "cos", "grid_ready": "1" } },
                // Group alpha (2 workers)
                { "pane_id": "w1:pA1", "tab_id": "w1:t1", "workspace_id": "w1", "agent_status": "idle", "tokens": { "grid_role": "worker", "grid_owner": "cos", "grid_group": "alpha", "grid_ready": "1" } },
                { "pane_id": "w1:pA2", "tab_id": "w1:t1", "workspace_id": "w1", "agent_status": "idle", "tokens": { "grid_role": "worker", "grid_owner": "cos", "grid_group": "alpha", "grid_ready": "1" } },
                // Group beta (2 workers)
                { "pane_id": "w1:pB1", "tab_id": "w1:t1", "workspace_id": "w1", "agent_status": "idle", "tokens": { "grid_role": "worker", "grid_owner": "cos", "grid_group": "beta", "grid_ready": "1" } },
                { "pane_id": "w1:pB2", "tab_id": "w1:t1", "workspace_id": "w1", "agent_status": "idle", "tokens": { "grid_role": "worker", "grid_owner": "cos", "grid_group": "beta", "grid_ready": "1" } }
            ],
            "layouts": [],
            "agents": []
        }
    });

    let snap = parse_snapshot_value(&val).unwrap();
    let ops = plan(&snap).unwrap();
    assert_eq!(ops.len(), 4);

    let mut driver = FakeHerdrDriver::new(snap);
    let res = apply_ops(&mut driver, &ops, None);
    assert!(
        res.is_ok(),
        "apply_ops must succeed across multiple groups: {:?}",
        res
    );
    assert_eq!(driver.executed_moves.len(), 4);

    // Both groups created their own tabs
    let move_a2 = driver
        .executed_moves
        .iter()
        .find(|m| m.0 == "w1:pA2")
        .unwrap();
    let move_b2 = driver
        .executed_moves
        .iter()
        .find(|m| m.0 == "w1:pB2")
        .unwrap();
    let tab_a = match &move_a2.1 {
        Target::Split { tab, .. } => tab.clone(),
        _ => panic!("expected split target for pA2"),
    };
    let tab_b = match &move_b2.1 {
        Target::Split { tab, .. } => tab.clone(),
        _ => panic!("expected split target for pB2"),
    };
    assert_ne!(
        tab_a, tab_b,
        "alpha and beta workers must split into different tabs"
    );
}

#[test]
fn test_check_filter_does_not_drop_chunk_0_splits_when_tab_ends_in_dash_two() {
    // Round 2 Finding 2 repro:
    // When GRID_TAB / group ends in -2 (e.g. web-2), chunk-0 split ops targeting
    // @workers must not be dropped by the check filter.
    let val = serde_json::json!({
        "snapshot": {
            "workspaces": [{ "workspace_id": "w1", "label": "main" }],
            "tabs": [
                { "tab_id": "w1:t1", "workspace_id": "w1", "label": "main", "pane_count": 2 },
                { "tab_id": "w1:t2", "workspace_id": "w1", "label": "web-2-workers", "pane_count": 1 }
            ],
            "panes": [
                { "pane_id": "w1:p1", "tab_id": "w1:t1", "workspace_id": "w1", "agent_status": "idle", "tokens": { "grid_role": "orchestrator", "grid_owner": "cos", "grid_ready": "1" } },
                // Worker 1 is in web-2-workers
                { "pane_id": "w1:p2", "tab_id": "w1:t2", "workspace_id": "w1", "agent_status": "idle", "tokens": { "grid_role": "worker", "grid_owner": "cos", "grid_group": "web-2", "grid_ready": "1" } },
                // Worker 2 is unarranged in main tab
                { "pane_id": "w1:p3", "tab_id": "w1:t1", "workspace_id": "w1", "agent_status": "idle", "tokens": { "grid_role": "worker", "grid_owner": "cos", "grid_group": "web-2", "grid_ready": "1" } }
            ],
            "layouts": [],
            "agents": []
        }
    });

    let snap = parse_snapshot_value(&val).unwrap();
    let ops = plan(&snap).unwrap();
    assert_eq!(ops.len(), 1);
    assert_eq!(ops[0].pane, "w1:p3");

    // filter_ops_for_check scoped with --tab web-2-workers must keep the pending split op!
    let filtered = quaqua::grid::reconcile::filter_ops_for_check(
        &ops,
        &snap,
        Some("cos"),
        None,
        Some("web-2-workers"),
        None,
    );
    assert_eq!(
        filtered.len(),
        1,
        "chunk-0 split op must not be dropped when tab label has -2 in base name"
    );
}

#[test]
fn test_fake_herdr_driver_common_lifecycle() {
    let snap = load_edge_fixture("1-worker");
    let mut driver = FakeHerdrDriver::new(snap);

    // Verify initial snapshot
    let s = driver.snapshot().expect("snapshot should succeed");
    assert_eq!(s.panes.len(), 2);

    // Perform NewTab move
    let res = driver
        .move_pane(
            "w1:p5",
            &Target::NewTab {
                label: "workers-tab".to_string(),
            },
        )
        .expect("move_pane NewTab should succeed");
    assert_eq!(res.previous_pane_id, Some("w1:p5".to_string()));
    assert!(res.tab_id.starts_with("w1:t_"));

    // Perform Rename
    driver
        .rename_pane(&res.pane_id, "renamed-worker")
        .expect("rename_pane should succeed");
    assert_eq!(driver.executed_renames.len(), 1);
    assert_eq!(driver.executed_renames[0].1, "renamed-worker");
}

#[test]
fn test_reconcile_step_convergence_matches_filter_ops_for_check() {
    // Test on working-deferred.json: worker is working, so plan has deferred op,
    // but filter_ops_for_check and reconcile_step must both agree that runnable ops = 0 (converged).
    let snap = load_edge_fixture("working-deferred");
    let ops = plan(&snap).expect("plan must succeed for working-deferred");
    assert!(!ops.is_empty(), "plan has deferred ops");
    assert!(
        ops.iter().all(|op| op.deferred),
        "all ops in working-deferred must be deferred"
    );

    let runnable =
        quaqua::grid::reconcile::filter_ops_for_check(&ops, &snap, None, None, None, None);
    assert!(
        runnable.is_empty(),
        "filter_ops_for_check must yield 0 runnable ops"
    );

    let sdir = edge_state_dir("test-deferred-convergence");
    let mut driver = FakeHerdrDriver::new(snap);
    let res = reconcile_step(&sdir, &mut driver, None);
    assert!(
        res.is_ok(),
        "reconcile_step must succeed on converged/deferred layout: {:?}",
        res
    );
    assert!(
        driver.executed_moves.is_empty(),
        "reconcile_step must not execute any moves when layout has 0 runnable ops"
    );
    let _ = fs::remove_dir_all(&sdir);
}

#[test]
fn test_unified_herdr_driver_instantiation_and_contract() {
    // CliHerdrDriver is an alias to quaqua::herdr::Herdr
    let mut driver = quaqua::grid::CliHerdrDriver::new();
    let expected_bin = std::env::var("HERDR_BIN_PATH").unwrap_or_else(|_| "herdr".to_string());
    assert_eq!(driver.bin, expected_bin);

    // Overridden binary via struct field
    driver.bin = "/nonexistent/herdr/binary".to_string();
    let snap_res = driver.snapshot();
    assert!(
        snap_res.is_err(),
        "snapshot with invalid binary path must return Err"
    );
    let err = snap_res.unwrap_err();
    assert!(
        err.contains("cannot run /nonexistent/herdr/binary"),
        "unexpected error: {err}"
    );
}

/// No planned op may move a pane into a tab of another workspace: herdr keeps
/// tabs workspace-local, so such an op can never run and the check that counts
/// it times out (the reported `wA:p1J -> new_tab quaqua-workers` pending op).
fn assert_ops_stay_in_their_workspace(snap: &Snapshot, ops: &[quaqua::grid::Op]) {
    for op in ops {
        let pane_ws = snap
            .panes
            .iter()
            .find(|p| p.pane_id == op.pane)
            .and_then(|p| p.workspace_id.as_deref());
        if let Target::Split { tab, .. } = &op.target {
            if let Some(tab_ws) = snap
                .tabs
                .iter()
                .find(|t| &t.tab_id == tab)
                .and_then(|t| t.workspace_id.as_deref())
            {
                assert_eq!(
                    Some(tab_ws),
                    pane_ws,
                    "op {op:?} moves a pane across workspaces"
                );
            }
        }
    }
}

/// A project Lead's worker must be arranged inside the Lead's own workspace even
/// when the pane still carries the default owner `cos` (a herdr restart restores
/// the shell env, not the pane's tag). The CoS's workers in w1 are arranged and
/// must stay untouched.
#[test]
fn test_plan_arranges_each_workspace_separately() {
    let val = serde_json::json!({
        "snapshot": {
            "workspaces": [
                { "workspace_id": "w1", "label": "quaqua" },
                { "workspace_id": "wA", "label": "arch" }
            ],
            "tabs": [
                { "tab_id": "w1:t1", "workspace_id": "w1", "label": "quaqua", "pane_count": 1 },
                { "tab_id": "w1:t2", "workspace_id": "w1", "label": "quaqua-workers", "pane_count": 2 },
                { "tab_id": "wA:t1", "workspace_id": "wA", "label": "arch", "pane_count": 1 },
                { "tab_id": "wA:t2", "workspace_id": "wA", "label": "scratch", "pane_count": 1 }
            ],
            "panes": [
                { "pane_id": "w1:p1", "tab_id": "w1:t1", "workspace_id": "w1", "agent_status": "idle", "tokens": { "grid_role": "orchestrator", "grid_owner": "cos", "grid_ready": "1" } },
                { "pane_id": "w1:p2", "tab_id": "w1:t2", "workspace_id": "w1", "agent_status": "idle", "tokens": { "grid_role": "worker", "grid_owner": "cos", "grid_group": "quaqua", "grid_tab": "quaqua-workers", "grid_ready": "1" } },
                { "pane_id": "w1:p3", "tab_id": "w1:t2", "workspace_id": "w1", "agent_status": "idle", "tokens": { "grid_role": "worker", "grid_owner": "cos", "grid_group": "quaqua", "grid_tab": "quaqua-workers", "grid_ready": "1" } },
                { "pane_id": "wA:p1", "tab_id": "wA:t1", "workspace_id": "wA", "agent_status": "idle", "tokens": { "grid_role": "orchestrator", "grid_owner": "arch-lead", "grid_ready": "1" } },
                { "pane_id": "wA:p2", "tab_id": "wA:t2", "workspace_id": "wA", "agent_status": "idle", "tokens": { "grid_role": "worker", "grid_owner": "cos", "grid_group": "quaqua", "grid_tab": "arch-workers", "grid_ready": "1" } }
            ],
            "layouts": [
                {
                    "tab_id": "w1:t2", "workspace_id": "w1", "area": { "x": 0, "y": 0, "width": 200, "height": 50 },
                    "panes": [
                        { "pane_id": "w1:p2", "rect": { "x": 0, "y": 0, "width": 100, "height": 50 } },
                        { "pane_id": "w1:p3", "rect": { "x": 100, "y": 0, "width": 100, "height": 50 } }
                    ]
                }
            ],
            "agents": []
        }
    });

    let snap = parse_snapshot_value(&val).unwrap();
    let ops = plan(&snap).unwrap();
    assert_ops_stay_in_their_workspace(&snap, &ops);

    // The Lead's unarranged worker is taken to its own anchor in wA, not into a
    // quaqua tab of the CoS workspace.
    assert_eq!(ops.len(), 1, "only the wA worker needs arranging: {ops:?}");
    assert_eq!(ops[0].pane, "wA:p2");
    match &ops[0].target {
        Target::Split { tab, anchor, .. } => {
            assert_eq!(
                tab, "wA:t1",
                "must land beside the Lead in its own workspace"
            );
            assert_eq!(anchor, "wA:p1");
        }
        other => panic!("expected the wA worker beside its Lead, got {other:?}"),
    }
}

/// A `grid check`/repair invoked from a Lead's workspace must ignore pending ops
/// of panes in other workspaces: that is what timed the check out (60 s) while
/// the Lead's own layout was valid.
#[test]
fn test_check_from_a_lead_workspace_ignores_foreign_workspace_panes() {
    let val = serde_json::json!({
        "snapshot": {
            "workspaces": [
                { "workspace_id": "w1", "label": "quaqua" },
                { "workspace_id": "wA", "label": "arch" }
            ],
            "tabs": [
                { "tab_id": "w1:t1", "workspace_id": "w1", "label": "quaqua", "pane_count": 3 },
                { "tab_id": "wA:t1", "workspace_id": "wA", "label": "arch", "pane_count": 2 }
            ],
            "panes": [
                { "pane_id": "w1:p1", "tab_id": "w1:t1", "workspace_id": "w1", "agent_status": "idle", "tokens": { "grid_role": "orchestrator", "grid_owner": "cos", "grid_ready": "1" } },
                { "pane_id": "w1:p2", "tab_id": "w1:t1", "workspace_id": "w1", "agent_status": "idle", "tokens": { "grid_role": "worker", "grid_owner": "cos", "grid_group": "quaqua", "grid_tab": "quaqua-workers", "grid_ready": "1" } },
                { "pane_id": "w1:p3", "tab_id": "w1:t1", "workspace_id": "w1", "agent_status": "idle", "tokens": { "grid_role": "worker", "grid_owner": "cos", "grid_group": "quaqua", "grid_tab": "quaqua-workers", "grid_ready": "1" } },
                { "pane_id": "wA:pLEAD9", "tab_id": "wA:t1", "workspace_id": "wA", "agent_status": "idle", "tokens": { "grid_role": "orchestrator", "grid_owner": "arch-lead", "grid_ready": "1" } },
                { "pane_id": "wA:p2", "tab_id": "wA:t1", "workspace_id": "wA", "agent_status": "idle", "tokens": { "grid_role": "worker", "grid_owner": "cos", "grid_group": "quaqua", "grid_tab": "arch-workers", "grid_ready": "1" } }
            ],
            "layouts": [
                {
                    "tab_id": "wA:t1", "workspace_id": "wA", "area": { "x": 0, "y": 0, "width": 200, "height": 50 },
                    "panes": [
                        { "pane_id": "wA:pLEAD9", "rect": { "x": 0, "y": 0, "width": 80, "height": 50 } },
                        { "pane_id": "wA:p2", "rect": { "x": 80, "y": 0, "width": 120, "height": 50 } }
                    ]
                }
            ],
            "agents": []
        }
    });

    let snap = parse_snapshot_value(&val).unwrap();
    let ops = plan(&snap).unwrap();
    assert_ops_stay_in_their_workspace(&snap, &ops);

    // The Lead pane invokes the check from wA while its shell still says `cos`.
    std::env::set_var("HERDR_PANE_ID", "wA:pLEAD9");
    let opts = ReconcileOptions {
        check: true,
        repair: false,
        tab: Some("arch-workers".to_string()),
        only: None,
        owner: None,
        dry_run: false,
        timeout_sec: Some(1),
    };
    let mut driver = FakeHerdrDriver::new(snap);
    let res = run_reconcile(&mut driver, &opts, "arch-workers", None);
    std::env::remove_var("HERDR_PANE_ID");
    assert!(
        res.is_ok(),
        "check must ignore other workspaces' pending ops: {res:?}"
    );
}

/// The grid readers (`hook`, `plan`, `reconcile`) run from an arbitrary cwd, so
/// team.toml must resolve like every other command: $QUAQUA_TEAM_TOML, then
/// ./team.toml, then <workspace_root>/team.toml (QUAQUA_HOME), then the
/// embedded default. A cwd-only read silently ignored ~/quaqua/team.toml.
#[test]
fn test_grid_plan_resolves_team_toml_from_the_workspace_root() {
    let root = std::env::temp_dir().join(format!("quaqua-grid-teamcwd-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let cwd = root.join("cwd");
    let workspace = root.join("workspace");
    fs::create_dir_all(&cwd).unwrap();
    fs::create_dir_all(&workspace).unwrap();

    let template = fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../templates/workspace/team.toml"),
    )
    .unwrap();
    let workspace_team = template.replace(
        "deferred_statuses = [\"working\"]",
        "deferred_statuses = [\"blocked\"]",
    );
    assert_ne!(
        workspace_team, template,
        "template must still carry the [grid] deferred_statuses default"
    );
    fs::write(workspace.join("team.toml"), &workspace_team).unwrap();

    // One worker (status "blocked") beside its owner's pane in a scratch tab:
    // the built-in default ("working") would not defer it, the workspace file
    // will. With one worker the planner emits one split op.
    let snapshot = serde_json::json!({
        "snapshot": {
            "workspaces": [{ "workspace_id": "w1", "label": "qa" }],
            "tabs": [
                { "tab_id": "w1:t1", "workspace_id": "w1", "label": "main", "pane_count": 1 },
                { "tab_id": "w1:t2", "workspace_id": "w1", "label": "scratch", "pane_count": 1 }
            ],
            "panes": [
                {
                    "pane_id": "w1:p1",
                    "tab_id": "w1:t1",
                    "workspace_id": "w1",
                    "agent": "claude",
                    "agent_status": "idle",
                    "tokens": {
                        "grid_role": "orchestrator",
                        "grid_owner": "cos",
                        "grid_ready": "1"
                    }
                },
                {
                    "pane_id": "w1:p5",
                    "tab_id": "w1:t2",
                    "workspace_id": "w1",
                    "agent": "pi",
                    "agent_status": "blocked",
                    "tokens": {
                        "grid_role": "worker",
                        "grid_owner": "cos",
                        "grid_group": "qa-workers",
                        "grid_ready": "1"
                    }
                }
            ],
            "layouts": [],
            "agents": []
        }
    });
    let snap_path = root.join("snapshot.json");
    fs::write(&snap_path, snapshot.to_string()).unwrap();

    let plan_from = |cwd: &PathBuf, home: &PathBuf| -> serde_json::Value {
        let out = Command::new(env!("CARGO_BIN_EXE_quaqua"))
            .args(["grid", "plan", "--file"])
            .arg(&snap_path)
            .current_dir(cwd)
            .env("QUAQUA_HOME", home)
            .env_remove("QUAQUA_TEAM_TOML")
            .env_remove("QUAQUA_ROOT")
            .output()
            .expect("failed to spawn quaqua grid plan");
        assert!(
            out.status.success(),
            "grid plan failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        serde_json::from_slice(&out.stdout).expect("grid plan must print ops JSON")
    };

    let ops = plan_from(&cwd, &workspace);
    assert_eq!(
        ops.as_array().map(Vec::len),
        Some(1),
        "one worker must yield one op: {ops}"
    );
    assert_eq!(
        ops[0]["deferred"], true,
        "deferred_statuses from <workspace>/team.toml must be used: {ops}"
    );

    // No behaviour change when the cwd holds a team.toml: it wins over the
    // workspace root (its list names "idle", so the same blocked worker is no
    // longer deferred).
    let cwd_team = template.replace(
        "deferred_statuses = [\"working\"]",
        "deferred_statuses = [\"idle\"]",
    );
    fs::write(cwd.join("team.toml"), cwd_team).unwrap();
    let ops = plan_from(&cwd, &workspace);
    assert_eq!(
        ops[0]["deferred"], false,
        "./team.toml must win over <workspace>/team.toml: {ops}"
    );

    let _ = fs::remove_dir_all(&root);
}
