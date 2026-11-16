//! The herdr CLI seam: [`HerdrClient`] is the trait, [`CliHerdr`] runs the real
//! binary, [`FakeHerdr`] replays it in memory. Only verbs listed in
//! `herdr --help` (0.9.5) appear here.

mod fake;
pub mod liveness;

pub use fake::FakeHerdr;

/// Error code for a start on a pane that already hosts another agent. Not a
/// herdr 0.9.5 documented code: T39's live probe may replace the value, so
/// classify by this const, never by the literal.
pub const PANE_BUSY: &str = "pane_busy";
/// Error code for a start whose name is already taken by an agent with a
/// different kind or pane (distinct from an occupied pane).
pub const NAME_CLASH: &str = "agent_name_clash";

/// The herdr verbs of [`HerdrClient`], for [`FakeHerdr`] fault/reply injection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Verb {
    Start,
    Get,
    List,
    Wait,
    Prompt,
    SendKeys,
    PaneSplit,
    PaneClose,
    PaneList,
    TabCreate,
    TabClose,
    TabList,
    PaneGet,
    PaneReportMetadata,
    PaneRead,
    PaneProcessInfo,
}

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

/// `Send + Sync`: drivers run on daemon threads while a test holds the same client.
pub trait HerdrClient: Send + Sync {
    /// `herdr agent start NAME --kind KIND --pane PANE --timeout MS [-- AGENT_ARG...]`;
    /// `agent_args` are the agent's own args (model, effort), without the `--`.
    fn agent_start(
        &self,
        name: &str,
        kind: &str,
        pane: &str,
        timeout_ms: u32,
        agent_args: &[&str],
    ) -> io::Result<Reply>;
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
    /// `herdr agent send-keys NAME KEY...` (`esc` is the canonical Escape).
    /// A client that cannot send keys reports `Unsupported`, so existing
    /// implementors keep compiling.
    fn agent_send_keys(&self, _name: &str, _keys: &[&str]) -> io::Result<Reply> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "agent send-keys not supported by this client",
        ))
    }
    /// `herdr notification show TITLE --body BODY [--sound SOUND]`: a desktop
    /// notification. Default `Unsupported`, like `agent_send_keys`.
    fn notification_show(
        &self,
        _title: &str,
        _body: &str,
        _sound: Option<&str>,
    ) -> io::Result<Reply> {
        Err(unsupported("notification show"))
    }
    /// `herdr pane split TARGET --direction right|down --no-focus`.
    /// Default `Unsupported`, like `agent_send_keys`.
    fn pane_split(&self, _target: &str, _dir: &str) -> io::Result<Reply> {
        Err(unsupported("pane split"))
    }
    /// `herdr pane split TARGET --direction right|down --cwd CWD --no-focus`: the new
    /// pane's shell starts in `cwd`. Default `Unsupported`.
    fn pane_split_in(&self, _target: &str, _dir: &str, _cwd: &str) -> io::Result<Reply> {
        Err(unsupported("pane split --cwd"))
    }
    /// `herdr pane close PANE`.
    fn pane_close(&self, _pane: &str) -> io::Result<Reply> {
        Err(unsupported("pane close"))
    }
    /// `herdr pane list`.
    fn pane_list(&self) -> io::Result<Reply> {
        Err(unsupported("pane list"))
    }
    /// `herdr tab create --label NAME --no-focus`.
    fn tab_create(&self, _name: &str) -> io::Result<Reply> {
        Err(unsupported("tab create"))
    }
    /// `herdr tab close TAB`.
    fn tab_close(&self, _tab: &str) -> io::Result<Reply> {
        Err(unsupported("tab close"))
    }
    /// `herdr tab list`: `{"result":{"tabs":[{tab_id,label,...}]}}`.
    fn tab_list(&self) -> io::Result<Reply> {
        Err(unsupported("tab list"))
    }
    /// `herdr pane get PANE`.
    fn pane_get(&self, _pane: &str) -> io::Result<Reply> {
        Err(unsupported("pane get"))
    }
    /// `herdr pane report-metadata PANE --source SOURCE [--token K=V]...
    /// [--display-agent TEXT]`. herdr merges tokens, so a repeat converges.
    fn pane_report_metadata(
        &self,
        _pane: &str,
        _source: &str,
        _tokens: &[(&str, &str)],
        _display: Option<&str>,
    ) -> io::Result<Reply> {
        Err(unsupported("pane report-metadata"))
    }
    /// `herdr pane read PANE --source recent-unwrapped --lines N`: the pane's
    /// recent text. The reply body is the text itself, not JSON.
    fn pane_read(&self, _pane: &str, _lines: u32) -> io::Result<Reply> {
        Err(unsupported("pane read"))
    }
    /// `herdr pane process-info --pane PANE`:
    /// `{"result":{"process_info":{"shell_pid":N,"foreground_processes":[{"pid":N,..}]}}}`.
    /// Default `Unsupported`.
    fn pane_process_info(&self, _pane: &str) -> io::Result<Reply> {
        Err(unsupported("pane process-info"))
    }
}

fn unsupported(what: &str) -> io::Error {
    io::Error::new(
        io::ErrorKind::Unsupported,
        format!("{what} not supported by this client"),
    )
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
        agent_args: &[&str],
    ) -> io::Result<Reply> {
        debug_assert!(
            agent_args.first() != Some(&"--"),
            "agent_args must not start with `--`; the separator is added here"
        );
        let t = timeout_ms.to_string();
        let mut args = argv(&[
            "agent",
            "start",
            name,
            "--kind",
            kind,
            "--pane",
            pane,
            "--timeout",
            &t,
        ]);
        if !agent_args.is_empty() {
            args.push("--".into());
            args.extend(agent_args.iter().map(|a| a.to_string()));
        }
        self.run(&args)
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

    fn agent_send_keys(&self, name: &str, keys: &[&str]) -> io::Result<Reply> {
        let mut args = argv(&["agent", "send-keys", name]);
        args.extend(keys.iter().map(|k| k.to_string()));
        self.run(&args)
    }

    fn notification_show(
        &self,
        title: &str,
        body: &str,
        sound: Option<&str>,
    ) -> io::Result<Reply> {
        let mut args = argv(&["notification", "show", title, "--body", body]);
        if let Some(s) = sound {
            args.push("--sound".into());
            args.push(s.into());
        }
        self.run(&args)
    }

    fn pane_split(&self, target: &str, dir: &str) -> io::Result<Reply> {
        self.run(&argv(&[
            "pane",
            "split",
            target,
            "--direction",
            dir,
            "--no-focus",
        ]))
    }

    fn pane_split_in(&self, target: &str, dir: &str, cwd: &str) -> io::Result<Reply> {
        self.run(&argv(&[
            "pane",
            "split",
            target,
            "--direction",
            dir,
            "--cwd",
            cwd,
            "--no-focus",
        ]))
    }

    fn pane_close(&self, pane: &str) -> io::Result<Reply> {
        self.run(&argv(&["pane", "close", pane]))
    }

    fn pane_list(&self) -> io::Result<Reply> {
        self.run(&argv(&["pane", "list"]))
    }

    fn tab_create(&self, name: &str) -> io::Result<Reply> {
        self.run(&argv(&["tab", "create", "--label", name, "--no-focus"]))
    }

    fn tab_close(&self, tab: &str) -> io::Result<Reply> {
        self.run(&argv(&["tab", "close", tab]))
    }

    fn tab_list(&self) -> io::Result<Reply> {
        self.run(&argv(&["tab", "list"]))
    }

    fn pane_get(&self, pane: &str) -> io::Result<Reply> {
        self.run(&argv(&["pane", "get", pane]))
    }

    fn pane_process_info(&self, pane: &str) -> io::Result<Reply> {
        self.run(&argv(&["pane", "process-info", "--pane", pane]))
    }

    fn pane_read(&self, pane: &str, lines: u32) -> io::Result<Reply> {
        self.run(&argv(&[
            "pane",
            "read",
            pane,
            "--source",
            "recent-unwrapped",
            "--lines",
            &lines.to_string(),
        ]))
    }

    fn pane_report_metadata(
        &self,
        pane: &str,
        source: &str,
        tokens: &[(&str, &str)],
        display: Option<&str>,
    ) -> io::Result<Reply> {
        let mut args = argv(&["pane", "report-metadata", pane, "--source", source]);
        for (k, v) in tokens {
            args.push("--token".into());
            args.push(format!("{k}={v}"));
        }
        if let Some(d) = display {
            args.push("--display-agent".into());
            args.push(d.into());
        }
        self.run(&args)
    }
}
