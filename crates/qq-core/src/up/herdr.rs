//! The herdr half of `quaqua up` (R1/R3): find or start the herdr session, find or
//! create the farm workspace, put the CoS in its root pane — resuming the recorded
//! session when there is one — then focus it and, outside herdr, attach the TUI.
//!
//! Every outside effect is one [`UpHost`] method, so the flow in [`super`] is
//! unit-tested against a fake and this module is the only place a real `herdr` runs.
//! The session name, the workspace label and the workspace root are derived from the
//! farm root ([`crate::db::path::workspace_root`]) by [`crate::identity::farm_of`]:
//! no path or name is written here.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::cli::{CliError, EXIT_UNREACHABLE};
use crate::config::CosLaunch;
use crate::db;
use crate::herdr::Reply;
use crate::identity;

/// The farm workspace: its herdr id and the pane in its root tab.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Workspace {
    pub id: String,
    pub root_pane: String,
}

/// A herdr agent that `agent list` still reports.
///
/// The pane is the identity: only an agent started with a name carries one, and the CoS
/// this launcher has to recognise may carry none, so nothing here keys on a name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Agent {
    /// The name given at `agent start`, when herdr reports one.
    pub name: Option<String>,
    pub pane: String,
    /// `agent_status`: `working`, `idle`, `blocked`, `done` or `unknown`.
    pub status: String,
    /// `agent_session.value`, when the entry carries one.
    pub session: Option<String>,
}

/// The first agent `agent list` reports for `pane`, from its raw JSON. A pure function so
/// the seam's answer is tested against the payload herdr really prints.
pub fn agent_in_pane_json(list: &str, pane: &str) -> Option<Agent> {
    let v: Value = serde_json::from_str(list).ok()?;
    let a = v
        .pointer("/result/agents")?
        .as_array()?
        .iter()
        .find(|a| a.get("pane_id").and_then(Value::as_str) == Some(pane))?;
    let text = |k: &str| a.get(k).and_then(Value::as_str).map(str::to_string);
    Some(Agent {
        name: text("name"),
        pane: pane.to_string(),
        status: text("agent_status").unwrap_or_default(),
        session: a
            .pointer("/agent_session/value")
            .or_else(|| a.get("agent_session"))
            .and_then(Value::as_str)
            .map(str::to_string),
    })
}

/// The outside world, one method per effect.
pub trait UpHost {
    /// The herdr session's name and the workspace label: both the farm root's last
    /// component, so neither is a literal here.
    fn name(&self) -> String;
    /// `HERDR_ENV` set: the caller is already inside a herdr pane.
    fn inside_herdr(&self) -> bool;
    /// The named session's server answers (`herdr --session NAME status server`).
    fn session_running(&mut self) -> bool;
    /// `herdr --session NAME server`, detached, then waited for.
    fn start_session(&mut self) -> Result<(), String>;
    /// The CoS launch from `[roles.cos]`, the error already shaped for the CLI.
    fn launch(&mut self) -> Result<CosLaunch, CliError>;
    /// The farm root every path here is derived from.
    fn workspace_root(&self) -> PathBuf;
    /// `(agent_session, pane_id)` from `runs/cos.identity.json`, if any.
    fn recorded(&mut self) -> Option<(String, String)>;
    /// The workspace carrying `label`, when it exists.
    fn workspace(&mut self, label: &str) -> Option<Workspace>;
    /// `herdr workspace create --cwd CWD --label LABEL --no-focus`.
    fn create_workspace(&mut self, label: &str, cwd: &Path) -> Result<Workspace, String>;
    /// The agent `herdr agent list` reports for `pane`, when it reports one. Named or
    /// not: the pane is what tells the CoS from everything else.
    fn agent_in_pane(&mut self, pane: &str) -> Option<Agent>;
    /// `herdr agent start NAME --kind KIND --pane PANE -- ARGS`.
    fn start_agent(
        &mut self,
        name: &str,
        kind: &str,
        pane: &str,
        args: &[String],
    ) -> Result<(), String>;
    /// Focus `pane`: the CoS may have no name to focus by.
    fn focus(&mut self, pane: &str) -> Result<(), String>;
    /// Attach the herdr TUI in this terminal; only called outside herdr.
    fn attach(&mut self) -> Result<(), String>;
    /// One line for the human.
    fn line(&mut self, s: &str);
}

/// `herdr agent start`'s readiness wait, the same budget a spawn gives it.
const START_TIMEOUT_MS: u32 = 60_000;

/// The real host: the herdr binary over its CLI, the store over `qq.db`, the terminal
/// over stdio. Outside herdr it adds `--session NAME` to every call and exports the
/// session and socket into the process env, so `service::install` — which reads them
/// from there — works from a plain terminal.
pub struct CliUp {
    pub bin: String,
    runs_dir: PathBuf,
    root: PathBuf,
    /// `(session, socket)` once resolved.
    resolved: Option<(String, String)>,
}

impl CliUp {
    pub fn new(runs_dir: impl Into<PathBuf>) -> Self {
        let runs_dir = runs_dir.into();
        let root = db::path::workspace_root();
        CliUp {
            bin: std::env::var("HERDR_BIN_PATH")
                .ok()
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| "herdr".to_string()),
            runs_dir,
            root,
            resolved: None,
        }
    }

    /// The session and the workspace label: the farm root's last component.
    pub fn name(&self) -> String {
        identity::farm_of(&self.root)
    }

    /// One herdr call. Inside herdr the ambient session answers; outside, the named one,
    /// with its session and socket in the child's environment too.
    fn herdr(&self, args: &[&str]) -> std::io::Result<Reply> {
        let mut cmd = Command::new(&self.bin);
        if !self.inside_herdr() {
            cmd.arg("--session").arg(self.name());
            if let Some((session, socket)) = &self.resolved {
                cmd.env("HERDR_SESSION", session)
                    .env("HERDR_SOCKET_PATH", socket);
            }
        }
        cmd.args(args);
        let out = cmd.output()?;
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

    /// `herdr status server --json` as JSON.
    fn status(&self) -> Option<Value> {
        let r = self.herdr(&["status", "server", "--json"]).ok()?;
        serde_json::from_str(&r.body).ok()
    }

    fn json(&self, args: &[&str]) -> Option<Value> {
        let r = self.herdr(args).ok().filter(|r| r.ok)?;
        serde_json::from_str(&r.body).ok()
    }

    /// Resolves `(session, socket)` and exports them: `service::install` reads the
    /// process env, so the install path works outside herdr.
    fn resolve_env(&mut self) -> Option<(String, String)> {
        if let Some(v) = &self.resolved {
            return Some(v.clone());
        }
        let v = self.status()?;
        let text = |k: &str| v.get(k).and_then(Value::as_str).map(str::to_string);
        let pair = (text("session")?, text("socket")?);
        // SAFETY: single-threaded at this point in `up`; the CLI runs no other thread.
        std::env::set_var("HERDR_SESSION", &pair.0);
        std::env::set_var("HERDR_SOCKET_PATH", &pair.1);
        self.resolved = Some(pair.clone());
        Some(pair)
    }

    /// The first pane in `workspace`: the root pane of a workspace whose root tab is its
    /// first tab, which is what `workspace create` makes.
    fn root_pane(&self, workspace: &str) -> Option<String> {
        let v = self.json(&["pane", "list"])?;
        v.pointer("/result/panes")?
            .as_array()?
            .iter()
            .find(|p| p.get("workspace_id").and_then(Value::as_str) == Some(workspace))
            .and_then(|p| p.get("pane_id").and_then(Value::as_str))
            .map(str::to_string)
    }

    /// One herdr call that must succeed; the error is herdr's own text.
    fn verb(&self, args: &[&str]) -> Result<(), String> {
        match self.herdr(args) {
            Ok(r) if r.ok => Ok(()),
            Ok(r) => Err(r.body.trim().to_string()),
            Err(e) => Err(e.to_string()),
        }
    }

    fn find_workspace(&mut self, label: &str) -> Option<Workspace> {
        let v = self.json(&["workspace", "list"])?;
        let id = v
            .pointer("/result/workspaces")?
            .as_array()?
            .iter()
            .find(|w| w.get("label").and_then(Value::as_str) == Some(label))
            .and_then(|w| w.get("workspace_id").and_then(Value::as_str))?
            .to_string();
        let root_pane = self.root_pane(&id).unwrap_or_default();
        Some(Workspace { id, root_pane })
    }
}

impl UpHost for CliUp {
    fn name(&self) -> String {
        CliUp::name(self)
    }

    fn inside_herdr(&self) -> bool {
        std::env::var("HERDR_ENV").is_ok_and(|v| !v.trim().is_empty())
    }

    fn session_running(&mut self) -> bool {
        if self.inside_herdr() {
            // We are in it: its server answers by definition.
            return true;
        }
        self.resolve_env().is_some()
            && self
                .status()
                .and_then(|v| v.get("running").and_then(Value::as_bool))
                .unwrap_or(false)
    }

    fn start_session(&mut self) -> Result<(), String> {
        let mut child = Command::new(&self.bin)
            .arg("--session")
            .arg(self.name())
            .arg("server")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| {
                format!(
                    "`{} --session {} server` did not start: {e}",
                    self.bin,
                    self.name()
                )
            })?;
        // The server is detached on purpose: `up` owns the CoS, not the server's life.
        let poll = crate::daemon::watch::REPORT_POLL;
        let deadline = Instant::now() + Duration::from_secs(crate::config::DAEMON_START_SEC);
        loop {
            if self.resolve_env().is_some() {
                self.resolved = None;
                if self.session_running() {
                    return Ok(());
                }
            }
            if let Ok(Some(st)) = child.try_wait() {
                let mut why = String::new();
                if let Some(mut e) = child.stderr.take() {
                    use std::io::Read;
                    let _ = e.read_to_string(&mut why);
                }
                if !self.session_running() {
                    return Err(format!(
                        "`{} --session {} server` exited {st}: {}",
                        self.bin,
                        self.name(),
                        why.trim()
                    ));
                }
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(format!(
                    "the herdr session '{}' did not answer within {}s",
                    self.name(),
                    crate::config::DAEMON_START_SEC
                ));
            }
            std::thread::sleep(poll);
        }
    }

    fn launch(&mut self) -> Result<CosLaunch, CliError> {
        let path = self.runs_dir.join(db::path::DB_FILE);
        let conn = db::open(&path)
            .map_err(|e| CliError::new(EXIT_UNREACHABLE, format!("{}: {e}", path.display())))?;
        super::launch_from(&conn)
    }

    fn workspace_root(&self) -> PathBuf {
        self.root.clone()
    }

    fn recorded(&mut self) -> Option<(String, String)> {
        identity::load_cos_identity(&self.runs_dir)
    }

    fn workspace(&mut self, label: &str) -> Option<Workspace> {
        self.find_workspace(label)
    }

    fn create_workspace(&mut self, label: &str, cwd: &Path) -> Result<Workspace, String> {
        let cwd = cwd.to_string_lossy().into_owned();
        let r = self
            .herdr(&[
                "workspace",
                "create",
                "--cwd",
                &cwd,
                "--label",
                label,
                "--no-focus",
            ])
            .map_err(|e| e.to_string())?;
        if !r.ok {
            return Err(format!("herdr workspace create refused: {}", r.body.trim()));
        }
        self.find_workspace(label)
            .ok_or_else(|| format!("herdr created no workspace labelled '{label}'"))
    }

    fn agent_in_pane(&mut self, pane: &str) -> Option<Agent> {
        let r = self.herdr(&["agent", "list"]).ok().filter(|r| r.ok)?;
        agent_in_pane_json(&r.body, pane)
    }

    fn start_agent(
        &mut self,
        name: &str,
        kind: &str,
        pane: &str,
        args: &[String],
    ) -> Result<(), String> {
        let t = START_TIMEOUT_MS.to_string();
        let mut argv: Vec<&str> = vec![
            "agent",
            "start",
            name,
            "--kind",
            kind,
            "--pane",
            pane,
            "--timeout",
            &t,
            "--",
        ];
        argv.extend(args.iter().map(String::as_str));
        let r = self.herdr(&argv).map_err(|e| e.to_string())?;
        if r.ok {
            return Ok(());
        }
        Err(r.body.trim().to_string())
    }

    /// `agent focus` takes an explicit pane id (herdr's skill: "an explicit pane ID, or a
    /// unique agent name"), so an unnamed CoS is focusable. A refusal falls back to the
    /// pane's tab and then its workspace: both are documented, pane-derived targets.
    fn focus(&mut self, pane: &str) -> Result<(), String> {
        if let Err(mut why) = self.verb(&["agent", "focus", pane]) {
            let v = self.json(&["pane", "get", pane]);
            let field = |k: &str| {
                v.as_ref()
                    .and_then(|v| v.pointer(&format!("/result/pane/{k}")))
                    .and_then(Value::as_str)
                    .map(str::to_string)
            };
            for (verb, id) in [
                ("tab", field("tab_id")),
                ("workspace", field("workspace_id")),
            ] {
                let Some(id) = id else { continue };
                match self.verb(&[verb, "focus", &id]) {
                    Ok(()) => return Ok(()),
                    Err(e) => why = e,
                }
            }
            return Err(why);
        }
        Ok(())
    }

    fn attach(&mut self) -> Result<(), String> {
        use std::io::IsTerminal;
        if !std::io::stdout().is_terminal() || !std::io::stdin().is_terminal() {
            self.line(&format!(
                "quaqua up: not attaching the herdr TUI (stdin is not a terminal); it is running in session '{}'",
                self.name()
            ));
            return Ok(());
        }
        let st = Command::new(&self.bin)
            .args(["session", "attach"])
            .arg(self.name())
            .status()
            .map_err(|e| format!("`{} session attach {}`: {e}", self.bin, self.name()))?;
        if st.success() {
            return Ok(());
        }
        Err(format!(
            "`{} session attach {}` exited {st}",
            self.bin,
            self.name()
        ))
    }

    fn line(&mut self, s: &str) {
        println!("{s}");
    }
}
