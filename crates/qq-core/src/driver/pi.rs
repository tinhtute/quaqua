//! The pi driver. Measured by T39: `esc` stops a working pi (fallbacks tried
//! in that order: esc, ctrl+c, esc esc, ctrl+c ctrl+c); the screen's `/session`
//! token counts are untrustworthy (0/0 after a stop), so usage comes from the
//! session jsonl herdr reports in `agent_session` (`kind:"path"`).
//!
//! Hook: pi's `tool_call` extension event can block a call (`{block:true,
//! reason}`); the installed extension asks `quaqua gate` and blocks on exit 2.

use super::{HarnessDriver, Hook, Usage};
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
const PUSH = /\bgit\b(\s+-\S+(\s+\S+)?)*\s+push\b/;
export default function (pi: any) {{
	pi.on("tool_call", async (event: any) => {{
		if (event.toolName !== "bash") return undefined;
		const command = String(event.input?.command ?? "");
		const payload = JSON.stringify({{ tool_name: "Bash", tool_input: {{ command }}, cwd: process.cwd() }});
		const r = spawnSync(GATE, ["gate"], {{ input: payload, encoding: "utf8" }});
		const blocked = r.error ? PUSH.test(command) : r.status === 2;
		if (!blocked) return undefined;
		return {{ block: true, reason: (r.stderr || "Blocked by quaqua gate.").trim() }};
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
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir)?;
            }
            let tmp = path.with_extension("ts.tmp");
            std::fs::write(&tmp, want)?;
            std::fs::rename(&tmp, &path)?;
        }
        Ok(Some(Hook { path }))
    }

    /// Sums every assistant `message.usage` line of the session jsonl:
    /// `input`, `output`, `cacheRead`, `cacheWrite` and `cost.total` (USD).
    fn usage(&self, session_file: &Path) -> io::Result<Option<Usage>> {
        let text = std::fs::read_to_string(session_file)?;
        let mut u = Usage::default();
        let mut seen = false;
        for line in text.lines() {
            let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
                continue; // a half-written last line
            };
            let Some(m) = v.pointer("/message/usage") else {
                continue;
            };
            seen = true;
            let n = |k: &str| m.get(k).and_then(|x| x.as_u64()).unwrap_or(0);
            u.input += n("input");
            u.output += n("output");
            u.cache_read += n("cacheRead");
            u.cache_write += n("cacheWrite");
            u.cost_usd += m
                .pointer("/cost/total")
                .and_then(|x| x.as_f64())
                .unwrap_or(0.0);
        }
        Ok(seen.then_some(u))
    }
}
