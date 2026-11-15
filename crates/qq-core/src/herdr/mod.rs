//! The herdr CLI seam: [`HerdrClient`] is the trait, [`CliHerdr`] runs the real
//! binary, [`FakeHerdr`] replays it in memory. Only verbs listed in
//! `herdr --help` (0.9.5) appear here.

mod fake;
pub mod liveness;

pub use fake::FakeHerdr;

use std::io;
use std::process::Command;

/// One herdr reply: exit success plus stdout (stderr when stdout is empty).
/// Server errors are JSON `{"error":{"code":..}}` on stderr with status 1.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reply {
    pub ok: bool,
    pub body: String,
}

impl Reply {
    pub fn ok(body: impl Into<String>) -> Self {
        Reply {
            ok: true,
            body: body.into(),
        }
    }

    pub fn err(code: &str) -> Self {
        Reply {
            ok: false,
            body: format!(r#"{{"error":{{"code":"{code}"}}}}"#),
        }
    }

    /// `error.code` of a failed reply, when the body carries one.
    pub fn error_code(&self) -> Option<String> {
        let v: serde_json::Value = serde_json::from_str(&self.body).ok()?;
        v.pointer("/error/code")?.as_str().map(str::to_string)
    }
}

pub trait HerdrClient {
    /// `herdr agent start NAME --kind KIND --pane PANE --timeout MS`.
    fn agent_start(&self, name: &str, kind: &str, pane: &str, timeout_ms: u32)
        -> io::Result<Reply>;
    /// `herdr agent get NAME`.
    fn agent_get(&self, name: &str) -> io::Result<Reply>;
    /// `herdr agent list`.
    fn agent_list(&self) -> io::Result<Reply>;
    /// `herdr agent wait NAME [--until S]... --timeout MS`; empty `until` is herdr's default set.
    fn agent_wait(&self, name: &str, until: &[&str], timeout_ms: u64) -> io::Result<Reply>;
    /// `herdr agent prompt NAME TEXT [--wait --until S... --timeout MS]`; `wait` is `(until, timeout_ms)`.
    fn agent_prompt(
        &self,
        name: &str,
        text: &str,
        wait: Option<(&[&str], u64)>,
    ) -> io::Result<Reply>;
}

/// The real binary: `HERDR_BIN_PATH` when set and non-empty, else `herdr`.
#[derive(Debug, Clone)]
pub struct CliHerdr {
    pub bin: String,
}

impl CliHerdr {
    pub fn new() -> Self {
        let bin = std::env::var("HERDR_BIN_PATH")
            .ok()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "herdr".to_string());
        CliHerdr { bin }
    }

    fn run(&self, args: &[String]) -> io::Result<Reply> {
        let out = Command::new(&self.bin).args(args).output()?;
        let stdout = String::from_utf8_lossy(&out.stdout).to_string();
        let body = if stdout.trim().is_empty() {
            String::from_utf8_lossy(&out.stderr).to_string()
        } else {
            stdout
        };
        Ok(Reply {
            ok: out.status.success(),
            body,
        })
    }
}

impl Default for CliHerdr {
    fn default() -> Self {
        Self::new()
    }
}

fn argv(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|s| s.to_string()).collect()
}

fn wait_args(args: &mut Vec<String>, until: &[&str], timeout_ms: u64) {
    for s in until {
        args.push("--until".into());
        args.push((*s).into());
    }
    args.push("--timeout".into());
    args.push(timeout_ms.to_string());
}

impl HerdrClient for CliHerdr {
    fn agent_start(
        &self,
        name: &str,
        kind: &str,
        pane: &str,
        timeout_ms: u32,
    ) -> io::Result<Reply> {
        let t = timeout_ms.to_string();
        self.run(&argv(&[
            "agent",
            "start",
            name,
            "--kind",
            kind,
            "--pane",
            pane,
            "--timeout",
            &t,
        ]))
    }

    fn agent_get(&self, name: &str) -> io::Result<Reply> {
        self.run(&argv(&["agent", "get", name]))
    }

    fn agent_list(&self) -> io::Result<Reply> {
        self.run(&argv(&["agent", "list"]))
    }

    fn agent_wait(&self, name: &str, until: &[&str], timeout_ms: u64) -> io::Result<Reply> {
        let mut args = argv(&["agent", "wait", name]);
        wait_args(&mut args, until, timeout_ms);
        self.run(&args)
    }

    fn agent_prompt(
        &self,
        name: &str,
        text: &str,
        wait: Option<(&[&str], u64)>,
    ) -> io::Result<Reply> {
        let mut args = argv(&["agent", "prompt", name, text]);
        if let Some((until, timeout_ms)) = wait {
            args.push("--wait".into());
            wait_args(&mut args, until, timeout_ms);
        }
        self.run(&args)
    }
}
