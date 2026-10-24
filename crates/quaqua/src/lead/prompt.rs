//! `quaqua lead prompt <slug> <msg>` — clear the input box of `<slug>-lead`,
//! submit the message, and verify it landed (input empty + agent working).
//!
//! Retro 19/10 (issue #55 item 1): `herdr agent send-keys enter` left text in
//! the Lead's input box and the prompt never ran. The submit step here is
//! `herdr agent prompt --wait --until working`, whose `agent_prompt_stalled`
//! answer is exactly "the text did not leave the box"; the follow-up read
//! confirms the status and that the box is empty.

use crate::herdr::HerdrOps;
use crate::liveness::{liveness, Liveness};
use serde_json::Value;
use std::time::{Duration, Instant};

pub const USAGE: &str = "\
# Usage: quaqua lead prompt <slug> <msg>
#   Clears the input box of the project Lead (<slug>-lead), sends <msg>, and
#   verifies it landed (input empty + agent status working).";

const DEFAULT_WAIT_MS: u64 = 5000;
const POLL_MS: u64 = 100;

pub fn prompt_lead<H: HerdrOps>(args: &[String], herdr: &H) -> Result<(), (i32, String)> {
    if args.len() < 2 {
        return Err((1, format!("{USAGE}\n")));
    }
    let slug = &args[0];
    if slug.is_empty()
        || !slug
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    {
        return Err((1, format!("{USAGE}\n")));
    }
    let msg = args[1..].join(" ");
    if msg.trim().is_empty() {
        return Err((1, format!("{USAGE}\n")));
    }
    let name = format!("{slug}-lead");

    let runs_dir = crate::worker::cleanup::default_runs_dir();
    let _ = std::fs::create_dir_all(&runs_dir);
    crate::layout::shadow(
        None,
        Some(herdr),
        crate::layout::Verb::Prompt,
        "lead",
        Some(&name),
        crate::layout::SpawnOrigin::Cli,
        None,
        Some(slug),
        &runs_dir,
        None,
    );

    // The Lead must exist and expose a pane to read back.
    let info = match liveness(herdr, &name) {
        Liveness::Alive(body) => body,
        Liveness::Down => {
            return Err((
                1,
                format!(
                    "error: no live lead '{name}' — start it with `quaqua lead spawn {slug}`\n"
                ),
            ))
        }
        Liveness::Unknown(why) => {
            return Err((
                1,
                format!(
                "error: cannot determine whether lead '{name}' is live ({why}); not prompting\n"
            ),
            ))
        }
    };
    let fields = parse_agent(&info);
    if fields.is_none() {
        return Err((1, format!("error: cannot parse lead '{name}' from herdr\n")));
    }
    let (_, pane) = fields.expect("checked above");
    if pane.is_empty() {
        return Err((1, format!("error: lead '{name}' has no pane\n")));
    }
    send_prompt(herdr, &name, &pane, &msg)
}

/// Clear the input box of `name`, submit `msg` with `--wait --until working`,
/// and verify it landed. The one submit path for a Lead message: `lead prompt`
/// and the kickoff `lead spawn --task` share it.
pub fn send_prompt<H: HerdrOps>(
    herdr: &H,
    name: &str,
    pane: &str,
    msg: &str,
) -> Result<(), (i32, String)> {
    // Clear any draft (Ctrl-U kills the line) so stale text cannot swallow the
    // Enter that submits this message.
    let cleared = herdr.agent_send_keys(name, "ctrl+u").map_err(|e| {
        (
            1,
            format!("error: cannot clear the input box of '{name}': {e}\n"),
        )
    })?;
    if !cleared.status.success() {
        return Err((
            1,
            format!(
                "error: cannot clear the input box of '{name}': {}\n",
                err_text(&cleared)
            ),
        ));
    }

    let wait_ms = env_ms("QUAQUA_LEAD_PROMPT_WAIT_MS", DEFAULT_WAIT_MS);
    let wait = wait_ms.to_string();
    let sent = herdr
        .run(&[
            "agent",
            "prompt",
            name,
            msg,
            "--wait",
            "--until",
            "working",
            "--timeout",
            &wait,
        ])
        .map_err(|e| (1, format!("error: cannot prompt '{name}': {e}\n")))?;
    if !sent.status.success() {
        return Err((
            1,
            format!(
                "error: prompt did not land on '{name}' — herdr rejected the submit: {}\n",
                err_text(&sent)
            ),
        ));
    }

    verify(herdr, name, pane, msg, wait_ms)
}

/// Poll status and the input box; pass only when the agent is `working` and
/// the message is no longer a draft.
fn verify<H: HerdrOps>(
    herdr: &H,
    name: &str,
    pane: &str,
    msg: &str,
    wait_ms: u64,
) -> Result<(), (i32, String)> {
    let window = Duration::from_millis(wait_ms.clamp(300, 1000));
    let deadline = Instant::now() + window;
    let mut last;
    loop {
        let (_, info) = herdr
            .agent_get(name)
            .map_err(|e| (1, format!("error: cannot re-read lead '{name}': {e}\n")))?;
        let status = parse_agent(&info)
            .map(|(status, _)| status)
            .unwrap_or_else(|| "unknown".to_string());
        let pane_text = herdr.pane_read(pane, "visible", 12).unwrap_or_default();
        let input_empty = input_box_empty(&pane_text, msg);
        if status == "working" && input_empty {
            println!("prompted {name} pane={pane} status=working");
            return Ok(());
        }
        last = format!(
            "status={status}, {}",
            if input_empty {
                "input empty"
            } else {
                "input box still shows the message"
            }
        );
        if Instant::now() >= deadline {
            return Err((
                1,
                format!(
                    "error: prompt to '{name}' did not land ({last}) — inspect with `herdr agent read {name} --source visible`\n"
                ),
            ));
        }
        std::thread::sleep(Duration::from_millis(POLL_MS));
    }
}

/// `(agent_status, pane_id)` from `herdr agent get` JSON.
fn parse_agent(info: &str) -> Option<(String, String)> {
    let value: Value = serde_json::from_str(info).ok()?;
    let agent = value.get("result")?.get("agent")?;
    Some((
        agent
            .get("agent_status")
            .and_then(|s| s.as_str())
            .unwrap_or("unknown")
            .to_string(),
        agent
            .get("pane_id")
            .and_then(|p| p.as_str())
            .unwrap_or("")
            .to_string(),
    ))
}

fn err_text(out: &std::process::Output) -> String {
    let stderr = String::from_utf8_lossy(&out.stderr);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let text = if stderr.trim().is_empty() {
        stdout.trim()
    } else {
        stderr.trim()
    };
    if text.is_empty() {
        format!("herdr exited {}", out.status)
    } else {
        text.to_string()
    }
}

fn env_ms(key: &str, default: u64) -> u64 {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(default)
}

/// True when the pane's tail (input box + status bar) no longer holds the
/// message. A draft that survived a failed submit sits on one of those lines;
/// a submitted prompt has scrolled up into the transcript. Very short messages
/// are not identifiable in terminal output, so only herdr's submit result
/// guards them.
pub fn input_box_empty(pane_text: &str, msg: &str) -> bool {
    let first = msg.lines().next().unwrap_or("").trim();
    let probe: String = first.chars().take(48).collect();
    if probe.chars().count() < 8 {
        return true;
    }
    pane_text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .rev()
        .take(3)
        .all(|line| !line.contains(&probe))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::os::unix::process::ExitStatusExt;
    use std::process::Output;

    struct FakeHerdr {
        status: &'static str,
        prompt_ok: bool,
        pane_text: &'static str,
        calls: RefCell<Vec<String>>,
    }

    impl FakeHerdr {
        fn new(status: &'static str, prompt_ok: bool, pane_text: &'static str) -> Self {
            FakeHerdr {
                status,
                prompt_ok,
                pane_text,
                calls: RefCell::new(Vec::new()),
            }
        }
    }

    fn out(ok: bool, text: &str) -> std::io::Result<Output> {
        Ok(Output {
            status: std::process::ExitStatus::from_raw(if ok { 0 } else { 1 }),
            stdout: text.as_bytes().to_vec(),
            stderr: text.as_bytes().to_vec(),
        })
    }

    impl HerdrOps for FakeHerdr {
        fn run(&self, args: &[&str]) -> std::io::Result<Output> {
            self.calls.borrow_mut().push(args.join(" "));
            out(
                self.prompt_ok,
                if self.prompt_ok {
                    ""
                } else {
                    "agent_prompt_stalled"
                },
            )
        }
        fn agent_list(&self) -> std::io::Result<String> {
            unimplemented!()
        }
        fn agent_get(&self, _name: &str) -> std::io::Result<(bool, String)> {
            Ok((
                true,
                format!(
                    r#"{{"result":{{"agent":{{"agent_status":"{}","pane_id":"%p"}}}}}}"#,
                    self.status
                ),
            ))
        }
        fn agent_read(&self, _n: &str, _s: &str, _l: u32) -> std::io::Result<String> {
            unimplemented!()
        }
        fn agent_start(
            &self,
            _n: &str,
            _k: &str,
            _p: &str,
            _t: u32,
            _a: &[&str],
        ) -> std::io::Result<Output> {
            unimplemented!()
        }
        fn pane_split(&self, _c: &str, _o: &str, _e: &[(&str, &str)]) -> std::io::Result<Output> {
            unimplemented!()
        }
        fn pane_close(&self, _p: &str) -> std::io::Result<Output> {
            unimplemented!()
        }
        fn pane_report_metadata(&self, _p: &str, _t: &[&str]) -> std::io::Result<Output> {
            unimplemented!()
        }
        fn pane_wait_output(&self, _p: &str, _m: &str, _t: u32) -> bool {
            unimplemented!()
        }
        fn pane_read(&self, _p: &str, _s: &str, _l: u32) -> std::io::Result<String> {
            Ok(self.pane_text.to_string())
        }
        fn pane_send_keys(&self, _p: &str, _k: &str) -> std::io::Result<Output> {
            unimplemented!()
        }
        fn agent_send_keys(&self, _n: &str, key: &str) -> std::io::Result<Output> {
            self.calls.borrow_mut().push(format!("send-keys {key}"));
            if key == "ctrl-u" {
                return out(
                    false,
                    "{\"code\":\"invalid_key\",\"message\":\"unsupported key ctrl-u\"}",
                );
            }
            out(true, "")
        }
        fn agent_wait_idle(&self, _n: &str, _t: u32) -> std::io::Result<Output> {
            unimplemented!()
        }
        fn agent_prompt_nowait(&self, _n: &str, _p: &str) -> std::io::Result<Output> {
            unimplemented!()
        }
        fn agent_prompt_wait_background(
            &self,
            _n: &str,
            _p: &str,
            _t: u32,
            _f: std::fs::File,
        ) -> std::io::Result<std::process::Child> {
            unimplemented!()
        }
    }

    fn args() -> Vec<String> {
        vec!["baro".into(), "please fix the gate".into()]
    }

    #[test]
    fn landed_prompt_clears_then_submits_with_wait() {
        let fake = FakeHerdr::new(
            "working",
            true,
            "thinking\n  ┃\n  Build auto\n  esc interrupt",
        );
        prompt_lead(&args(), &fake).expect("landed");
        let calls = fake.calls.borrow();
        assert_eq!(calls[0], "send-keys ctrl+u");
        assert!(
            calls[1]
                .starts_with("agent prompt baro-lead please fix the gate --wait --until working"),
            "{}",
            calls[1]
        );
    }

    #[test]
    fn a_draft_left_in_the_input_box_fails() {
        let fake = FakeHerdr::new(
            "idle",
            true,
            "  ┃ please fix the gate\n  Build auto\n  esc interrupt",
        );
        let err = prompt_lead(&args(), &fake).expect_err("stuck");
        assert!(err.1.contains("did not land"), "{}", err.1);
        assert!(err.1.contains("input box"), "{}", err.1);
    }

    #[test]
    fn a_rejected_submit_names_the_reason() {
        let fake = FakeHerdr::new("blocked", false, "");
        let err = prompt_lead(&args(), &fake).expect_err("rejected");
        assert!(err.1.contains("did not land"), "{}", err.1);
        assert!(err.1.contains("agent_prompt_stalled"), "{}", err.1);
    }

    #[test]
    fn input_box_detection() {
        let msg = "please fix the gate now";
        assert!(!input_box_empty(
            "  ┃ please fix the gate now\n  Build\n  esc",
            msg
        ));
        assert!(input_box_empty("done\n  ┃\n  Build\n  esc", msg));
    }
}
