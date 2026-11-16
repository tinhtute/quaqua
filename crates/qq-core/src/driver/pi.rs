//! The pi driver. Measured by T39: `esc` stops a working pi (fallbacks tried
//! in that order: esc, ctrl+c, esc esc, ctrl+c ctrl+c); the screen's `/session`
//! token counts are untrustworthy (0/0 after a stop), so usage comes from the
//! session jsonl herdr reports in `agent_session` (`kind:"path"`).
//!
//! Hook: pi's `tool_call` extension event can block a call (`{block:true,
//! reason}`); the installed extension asks `quaqua gate` and blocks unless it exits 0 (fail closed, also on spawn error).

use super::{exclude_from_git, write_atomic, HarnessDriver, Hook, Usage};
use std::io;
use std::path::Path;

pub struct Pi;

/// Relative to the worktree; `launch_args` loads it with `-e`.
const HOOK_REL: &str = ".qq/pi-gate.ts";

fn hook_source(gate: &Path) -> String {
    format!(
        r#"// Written by quaqua (driver pi). Do not edit.
import {{ spawnSync }} from "node:child_process";
const GATE = {gate:?};
export default function (pi: any) {{
	pi.on("tool_call", async (event: any) => {{
		if (event.toolName !== "bash") return undefined;
		const command = String(event.input?.command ?? "");
		const payload = JSON.stringify({{ tool_name: "Bash", tool_input: {{ command }}, cwd: process.cwd() }});
		const r = spawnSync(GATE, ["gate"], {{ input: payload, encoding: "utf8" }});
		if (!r.error && r.status === 0) return undefined;
		return {{ block: true, reason: String(r.stderr || "Blocked by quaqua gate.").trim() }};
	}});
}}
"#
    )
}

impl HarnessDriver for Pi {
    fn harness(&self) -> &str {
        "pi"
    }

    fn herdr_kind(&self) -> &str {
        "pi"
    }

    fn launch_args(&self, model: &str, effort: Option<&str>) -> Vec<String> {
        let mut out = vec!["--model".to_string(), model.to_string()];
        if let Some(e) = effort {
            out.extend(["--thinking".to_string(), e.to_string()]);
        }
        out.extend(["-e".to_string(), HOOK_REL.to_string()]);
        out
    }

    fn stop_sequences(&self) -> Vec<Vec<String>> {
        [
            &["esc"][..],
            &["ctrl+c"],
            &["esc", "esc"],
            &["ctrl+c", "ctrl+c"],
        ]
        .iter()
        .map(|keys| keys.iter().map(|k| k.to_string()).collect())
        .collect()
    }

    fn install_hook(&self, worktree: &Path, gate: &Path) -> io::Result<Option<Hook>> {
        let path = worktree.join(HOOK_REL);
        let want = hook_source(gate);
        if std::fs::read_to_string(&path).ok().as_deref() != Some(want.as_str()) {
            write_atomic(&path, want.as_bytes())?;
        }
        exclude_from_git(worktree, &format!("/{HOOK_REL}"))?;
        Ok(Some(Hook { path }))
    }

    /// Sums the `message.usage` of every assistant message entry of the
    /// session jsonl: `input`, `output`, `cacheRead`, `cacheWrite` and
    /// `cost.total` (USD). An assistant entry without a cost makes the cost
    /// unknown (`None`), never $0.
    fn usage(&self, session_file: &Path) -> io::Result<Option<Usage>> {
        let text = std::fs::read_to_string(session_file)?;
        let mut u = Usage::default();
        let mut cost = Some(0.0);
        let mut seen = false;
        for line in text.lines() {
            let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
                continue; // a half-written last line
            };
            if v.pointer("/message/role").and_then(|r| r.as_str()) != Some("assistant") {
                continue;
            }
            let Some(m) = v.pointer("/message/usage") else {
                continue;
            };
            seen = true;
            let n = |k: &str| m.get(k).and_then(|x| x.as_u64()).unwrap_or(0);
            u.input += n("input");
            u.output += n("output");
            u.cache_read += n("cacheRead");
            u.cache_write += n("cacheWrite");
            cost = match (cost, m.pointer("/cost/total").and_then(|x| x.as_f64())) {
                (Some(t), Some(c)) => Some(t + c),
                _ => None,
            };
        }
        u.cost_usd = cost;
        Ok(seen.then_some(u))
    }
}
