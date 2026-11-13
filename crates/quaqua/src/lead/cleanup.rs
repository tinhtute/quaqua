//! Project Lead cleanup logic (`quaqua lead cleanup <slug>`).

use crate::herdr::HerdrOps;
use crate::liveness::{liveness, Liveness};
use crate::state::WorkerMetadata;
use crate::workspace::workspace_root_checked;
use serde_json::Value;
use std::path::PathBuf;

pub const USAGE: &str = "\
# Usage: quaqua lead cleanup <slug>
#   Tear down the project Lead <slug>-lead once the project is done. Refuses
#   while any of its Peers is still registered with herdr (runs/<peer>.owner
#   naming the Lead) — clean those up first with quaqua worker cleanup. Closes the
#   Lead's team workspace only when it is the one quaqua lead spawn made (label
#   <slug>) and not the caller's; a Lead living anywhere else (older spawns)
#   gets just its pane closed. Branch quaqua/<slug>-integ is kept; the
#   projects/INDEX.md owner cell returns to cos (serialized with quaqua lead spawn
#   via projects/INDEX.md.lock.d, published by atomic rename). A Peer or Lead
#   whose `herdr agent get` fails for any reason other than a definite
#   agent_not_found is of unknown liveness: cleanup aborts rather than close
#   a workspace it cannot prove is dead. A Lead with no herdr agent that still
#   left a trace (INDEX owner cell or runs/ files) is an orphan: cleaned with
#   nothing closed. No agent and no trace is refused as a typo.";

fn is_valid_cleanup_slug(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c.is_ascii_lowercase() || c.is_ascii_digit() => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// True when a Lead left anything behind: its INDEX owner cell or a runs/ file.
fn has_lead_trace(
    root: &std::path::Path,
    runs_dir: &std::path::Path,
    slug: &str,
    name: &str,
) -> bool {
    let files = [
        format!("{name}.rules.md"),
        format!("{name}.owner"),
        format!(".{name}.playbook"),
    ];
    if files.iter().any(|f| runs_dir.join(f).exists())
        || crate::layout::layout_role_path(runs_dir, name).exists()
    {
        return true;
    }
    std::fs::read_to_string(root.join("projects/INDEX.md"))
        .map(|t| {
            t.lines().any(|l| {
                let c: Vec<&str> = l.split('|').map(str::trim).collect();
                c.len() > 3 && c[1] == slug && c[3] == name
            })
        })
        .unwrap_or(false)
}

pub fn cleanup_lead<H: HerdrOps>(args: &[String], herdr: &H) -> Result<(), (i32, String)> {
    if args.len() != 1 {
        return Err((1, format!("{}\n", USAGE)));
    }
    let slug = &args[0];
    if !is_valid_cleanup_slug(slug) {
        return Err((1, format!("{}\n", USAGE)));
    }

    let root = workspace_root_checked()?;
    let runs_dir = std::env::var("QUAQUA_RUNS_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| root.join("runs"));
    let name = format!("{slug}-lead");

    // Check Lead liveness. A Lead with no herdr agent is an orphan when it
    // still left a trace (INDEX owner cell or runs/ files): it is cleaned
    // without closing anything. No trace at all is a typo and stays refused.
    let (pane, ws) = match liveness(herdr, &name) {
        Liveness::Alive(lead_body) => {
            let lead_json = serde_json::from_str::<Value>(&lead_body).ok();
            let agent_obj = lead_json
                .as_ref()
                .and_then(|j| j.get("result"))
                .and_then(|r| r.get("agent"))
                .unwrap();
            let pane = agent_obj
                .get("pane_id")
                .and_then(|p| p.as_str())
                .unwrap_or("")
                .to_string();
            let ws = agent_obj
                .get("workspace_id")
                .and_then(|w| w.as_str())
                .map(str::to_string)
                .unwrap_or_else(|| pane.split(':').next().unwrap_or("").to_string());
            (pane, ws)
        }
        Liveness::Down => {
            if !has_lead_trace(&root, &runs_dir, slug, &name) {
                return Err((1, format!("no live Lead '{name}'\n")));
            }
            (String::new(), String::new())
        }
        Liveness::Unknown(why) => {
            return Err((
                1,
                format!(
                    "cannot determine the state of Lead '{name}' ({why}) — refusing to clean up\n"
                ),
            ))
        }
    };
    let orphan = pane.is_empty() && ws.is_empty();

    // Check live Peers
    let mut live_peers = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&runs_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) == Some("owner") {
                let peer = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
                if !peer.is_empty() && WorkerMetadata::load_owner(&runs_dir, peer) == name {
                    match liveness(herdr, peer) {
                        Liveness::Alive(_) => live_peers.push(peer.to_string()),
                        Liveness::Down => {}
                        Liveness::Unknown(why) => {
                            return Err((
                                1,
                                format!(
                                    "cannot determine whether Peer '{peer}' is still live ({why}) — refusing to clean up '{name}'; retry, or run quaqua worker cleanup if it is gone\n"
                                ),
                            ));
                        }
                    }
                }
            }
        }
    }

    if !live_peers.is_empty() {
        return Err((
            1,
            format!(
                "{name} still owns live Peers: {} — run cleanup-worker.sh first\n",
                live_peers.join(" ")
            ),
        ));
    }

    let closed = if orphan {
        "orphan: no agent to close".to_string()
    } else {
        let caller_pane = match std::env::var("HERDR_PANE_ID") {
            Ok(p) => p,
            Err(_) => {
                let out = herdr
                    .run(&["pane", "current"])
                    .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
                    .unwrap_or_default();
                let cur_json = serde_json::from_str::<Value>(&out).ok();
                cur_json
                    .as_ref()
                    .and_then(|j| j.get("result"))
                    .and_then(|r| r.get("pane"))
                    .and_then(|p| p.get("pane_id"))
                    .and_then(|p| p.as_str())
                    .unwrap_or("")
                    .to_string()
            }
        };
        let caller_ws = caller_pane.split(':').next().unwrap_or("");

        let ws_out = herdr
            .run(&["workspace", "get", &ws])
            .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
            .unwrap_or_default();
        let ws_parsed = serde_json::from_str::<Value>(&ws_out).ok();
        let ws_label = ws_parsed
            .as_ref()
            .and_then(|j| j.get("result"))
            .and_then(|r| r.get("workspace"))
            .and_then(|w| w.get("label"))
            .and_then(|w| w.as_str())
            .unwrap_or("");
        if ws_label == slug && ws != caller_ws {
            let _ = herdr.run(&["workspace", "close", &ws]);
            format!("workspace {ws}")
        } else {
            let _ = herdr.run(&["pane", "close", &pane]);
            format!("pane {pane}")
        }
    };

    let rf = runs_dir.join(format!("{name}.rules.md"));
    let _ = std::fs::remove_file(rf);
    let _ = std::fs::remove_file(WorkerMetadata::owner_path(&runs_dir, &name));
    let _ = std::fs::remove_file(runs_dir.join(format!(".{name}.playbook")));
    let _ = std::fs::remove_file(crate::layout::layout_role_path(&runs_dir, &name));

    let index = root.join("projects/INDEX.md");
    if index.is_file() {
        crate::lead::spawn::update_project_index_owner(&root, slug, "cos")?;
    }

    println!("cleaned {name} ({closed} closed; branch quaqua/{slug}-integ kept)");
    Ok(())
}
