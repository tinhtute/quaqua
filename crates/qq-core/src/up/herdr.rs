//! The herdr half of `quaqua up` (R1/R3): find or start the herdr session, find or
//! create the farm workspace, choose the pane that hosts the CoS — from the real pane
//! list, never from a name — then focus it and, outside herdr, attach the TUI.
//!
//! Every outside effect is one [`UpHost`] method, so the flow in [`super`] is unit-tested
//! against a fake and this module is the only place a real `herdr` runs. The session name,
//! the workspace label and the workspace root are derived from the farm root
//! ([`crate::db::path::workspace_root`]) by [`crate::identity::farm_of`]: no path or name
//! is written here.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::cli::{CliError, EXIT_UNREACHABLE};
use crate::config::CosLaunch;
use crate::daemon::service::{HerdrEnv, HerdrFacts};
use crate::db;
use crate::herdr::{HerdrClient, Reply};
use crate::identity;

/// The farm workspace: its herdr id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Workspace {
    pub id: String,
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

/// The pane that hosts — or would host — the CoS, and whether an agent is already in it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CosPane {
    pub pane: String,
    pub live: bool,
}

/// The three `herdr` listings the CoS pane is chosen from, as they arrive.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PaneLists {
    pub panes: String,
    pub tabs: String,
    pub agents: String,
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

/// The `grid_role` token of a pane, when herdr reports one.
fn grid_role(p: &Value) -> Option<&str> {
    p.pointer("/tokens/grid_role").and_then(Value::as_str)
}

fn pane_id(p: &Value) -> Option<&str> {
    p.get("pane_id").and_then(Value::as_str)
}

/// A pane of one workspace, in the order the selector considers it.
struct Pane {
    id: String,
    tab: String,
    role: Option<String>,
}

/// The pane that hosts — or would host — the CoS of `workspace`, chosen from the real
/// listings:
///
/// 1. the pane tagged `grid_role=cos` (the CoS's own pane, tagged by `quaqua identity`);
/// 2. else the recorded pane from `runs/cos.identity.json`, when the agent session in it
///    is the recorded one (a stale record must not win);
/// 3. else the first pane of the workspace's first tab.
///
/// A pane tagged `grid_role=worker` is never the CoS: calling a worker the CoS leaves the
/// farm without one while `up` reports success. Tab order comes from `tab list`, so the
/// pane list's own order cannot change the answer. `Err` names the workspace when it has
/// no pane, or only workers.
pub fn cos_pane(
    lists: &PaneLists,
    workspace: &str,
    recorded: Option<&(String, String)>,
) -> Result<CosPane, String> {
    let panes = parse_panes(&lists.panes, workspace);
    if panes.is_empty() {
        return Err(format!(
            "workspace {workspace} has no pane to put the CoS in (herdr pane list named none)"
        ));
    }
    let order = tab_order(&lists.tabs, workspace);
    let position = |p: &Pane| order.iter().position(|t| *t == p.tab).unwrap_or(usize::MAX);
    let mut sorted: Vec<&Pane> = panes.iter().collect();
    // Stable sort on the tab index only: within a tab, the pane list's own order stays.
    sorted.sort_by_key(|p| position(p));
    let cands: Vec<&Pane> = sorted
        .into_iter()
        .filter(|p| p.role.as_deref() != Some("worker"))
        .collect();
    if cands.is_empty() {
        return Err(format!(
            "every pane of workspace {workspace} hosts a worker (grid_role=worker); close one or give the CoS its own tab"
        ));
    }
    let live = |pane: &str| agent_in_pane_json(&lists.agents, pane);
    if let Some(p) = cands.iter().find(|p| p.role.as_deref() == Some("cos")) {
        return Ok(CosPane {
            live: live(&p.id).is_some(),
            pane: p.id.clone(),
        });
    }
    if let Some((session, recorded_pane)) = recorded {
        if cands.iter().any(|p| &p.id == recorded_pane) {
            let matches =
                live(recorded_pane).is_some_and(|a| a.session.as_deref() == Some(session.as_str()));
            if matches {
                return Ok(CosPane {
                    pane: recorded_pane.clone(),
                    live: true,
                });
            }
        }
    }
    let first = cands[0];
    Ok(CosPane {
        live: live(&first.id).is_some(),
        pane: first.id.clone(),
    })
}

/// The panes of `workspace`, in `pane list` order.
fn parse_panes(list: &str, workspace: &str) -> Vec<Pane> {
    let Ok(v) = serde_json::from_str::<Value>(list) else {
        return Vec::new();
    };
    v.pointer("/result/panes")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter(|p| p.get("workspace_id").and_then(Value::as_str) == Some(workspace))
                .filter_map(|p| {
                    Some(Pane {
                        id: pane_id(p)?.to_string(),
                        tab: p.get("tab_id").and_then(Value::as_str)?.to_string(),
                        role: grid_role(p).map(str::to_string),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The tab ids of `workspace`, in `tab list` order. Tab ids are workspace-qualified
/// (`w1:t7`), and an empty answer falls back to the pane list's order.
fn tab_order(list: &str, workspace: &str) -> Vec<String> {
    let Ok(v) = serde_json::from_str::<Value>(list) else {
        return Vec::new();
    };
    v.pointer("/result/tabs")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|t| t.get("tab_id").and_then(Value::as_str))
                .filter(|t| t.split(':').next() == Some(workspace))
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
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
    /// The pane that hosts the CoS of `workspace`, or the one that would host it.
    fn cos_pane(
        &mut self,
        workspace: &str,
        recorded: Option<&(String, String)>,
    ) -> Result<CosPane, String>;
    /// The agent `herdr agent list` reports for `pane`, when it reports one.
    fn agent_in_pane(&mut self, pane: &str) -> Option<Agent>;
    /// `herdr agent start NAME --kind KIND --pane PANE -- ARGS`. `dialog`: accept the
    /// harness's folder-trust dialog when the start reports `agent_not_ready`, the same
    /// step a worker spawn takes. Never called into a pane that already hosts an agent.
    fn start_agent(
        &mut self,
        name: &str,
        kind: &str,
        pane: &str,
        args: &[String],
        dialog: bool,
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

/// Where the detached herdr server's own output goes: a log in the farm's runs dir, so a
/// full pipe can never block it and nothing writes into a closed one.
pub const SERVER_LOG: &str = "herdr-server.log";

/// The real host: the herdr binary over its CLI, the store over `qq.db`, the terminal
/// over stdio. It addresses the configured session by name on every call — inside herdr or
/// not — and exports the session and socket into the process env, so `service::install`,
/// which reads them from there, works from a plain terminal.
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

    /// The farm root this host derives the session name and the label from.
    pub fn set_root(&mut self, root: impl Into<PathBuf>) {
        self.root = root.into();
    }

    /// The session and the workspace label: the farm root's last component.
    pub fn name(&self) -> String {
        identity::farm_of(&self.root)
    }

    /// How long a herdr session (and the daemon) may take to answer: the setting
    /// `notify.daemon_start_sec`, else its default.
    pub fn budget(&self) -> Duration {
        let path = self.runs_dir.join(db::path::DB_FILE);
        db::open(&path).map_or_else(
            |_| Duration::from_secs(crate::config::DAEMON_START_SEC),
            |c| crate::config::daemon_start_sec(&c),
        )
    }

    /// One herdr call. The configured session is named on the command line, so a run from
    /// another session's pane still reaches this farm's.
    fn herdr(&self, args: &[&str]) -> std::io::Result<Reply> {
        let mut cmd = Command::new(&self.bin);
        cmd.arg("--session").arg(self.name());
        if let Some((session, socket)) = &self.resolved {
            cmd.env("HERDR_SESSION", session)
                .env("HERDR_SOCKET_PATH", socket);
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

    /// The body of one listing that must succeed.
    fn listing(&self, args: &[&str]) -> Result<String, String> {
        let r = self.herdr(args).map_err(|e| e.to_string())?;
        if r.ok {
            return Ok(r.body);
        }
        Err(format!("herdr {}: {}", args.join(" "), r.body.trim()))
    }

    /// Resolves `(session, socket)` through the resolver `daemon install` shares — the env
    /// first, then herdr — and exports them: the install path reads the process env.
    fn resolve_env(&mut self) -> Option<(String, String)> {
        if let Some(v) = &self.resolved {
            return Some(v.clone());
        }
        // The farm's session is the one to ask herdr about by name: every call below names it.
        let preferred = self.name();
        let facts = CliFacts { up: self };
        let resolved = HerdrEnv::resolve(Some(&preferred), &facts).ok()?;
        // The CLI path of `up` is single-threaded; see the module's `start_session` note.
        std::env::set_var("HERDR_SESSION", &resolved.session);
        std::env::set_var("HERDR_SOCKET_PATH", &resolved.socket);
        let pair = (resolved.session, resolved.socket);
        self.resolved = Some(pair.clone());
        Some(pair)
    }

    /// One herdr call that must succeed; the error is herdr's own text.
    fn verb(&self, args: &[&str]) -> Result<(), String> {
        match self.herdr(args) {
            Ok(r) if r.ok => Ok(()),
            Ok(r) => Err(r.body.trim().to_string()),
            Err(e) => Err(e.to_string()),
        }
    }
}

/// `up`'s herdr seam for the resolver `daemon install` shares: every call names the farm's
/// session, so a status is asked about that session and the listing is the plain one.
struct CliFacts<'a> {
    up: &'a CliUp,
}

impl HerdrFacts for CliFacts<'_> {
    fn server(&self, _session: Option<&str>) -> Option<Value> {
        // `herdr --session <farm> status server --json`, which is what `up` always asks.
        self.up.status()
    }

    fn sessions(&self) -> Option<Value> {
        self.up.json(&["session", "list", "--json"])
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
        self.resolve_env().is_some()
            && self
                .status()
                .and_then(|v| v.get("running").and_then(Value::as_bool))
                .unwrap_or(false)
    }

    /// Starts the server detached from `up`: its own process group (a Ctrl-C at the y/N
    /// prompt must not take the server, the CoS and every worker with it) and stdio that
    /// never blocks it or breaks on a closed end (a log in the runs dir).
    fn start_session(&mut self) -> Result<(), String> {
        let path = self.runs_dir.join(SERVER_LOG);
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let log = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .and_then(|f| f.try_clone().map(|g| (f, g)));
        let (out, err) = match log {
            Ok((a, b)) => (Stdio::from(a), Stdio::from(b)),
            // No log is better than a pipe nobody drains.
            Err(_) => (Stdio::null(), Stdio::null()),
        };
        let mut cmd = Command::new(&self.bin);
        cmd.arg("--session")
            .arg(self.name())
            .arg("server")
            .stdin(Stdio::null())
            .stdout(out)
            .stderr(err);
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            cmd.process_group(0);
        }
        let mut child = cmd.spawn().map_err(|e| {
            format!(
                "`{} --session {} server` did not start: {e}",
                self.bin,
                self.name()
            )
        })?;
        let poll = crate::daemon::watch::REPORT_POLL;
        let deadline = Instant::now() + self.budget();
        loop {
            // A fresh `status` after the spawn: the answer may predate the socket.
            self.resolved = None;
            if self.session_running() {
                return Ok(());
            }
            if let Ok(Some(st)) = child.try_wait() {
                let tail = std::fs::read_to_string(&path)
                    .map(|t| {
                        t.lines()
                            .rev()
                            .take(3)
                            .collect::<Vec<_>>()
                            .into_iter()
                            .rev()
                            .collect::<Vec<_>>()
                            .join(" | ")
                    })
                    .unwrap_or_default();
                if self.session_running() {
                    return Ok(());
                }
                return Err(format!(
                    "`{} --session {} server` exited {st}: {}",
                    self.bin,
                    self.name(),
                    tail
                ));
            }
            if Instant::now() >= deadline {
                return Err(format!(
                    "the herdr session '{}' did not answer within {}s (see {})",
                    self.name(),
                    self.budget().as_secs(),
                    path.display()
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
        let v = self.json(&["workspace", "list"])?;
        let id = v
            .pointer("/result/workspaces")?
            .as_array()?
            .iter()
            .find(|w| w.get("label").and_then(Value::as_str) == Some(label))
            .and_then(|w| w.get("workspace_id").and_then(Value::as_str))?
            .to_string();
        Some(Workspace { id })
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
        self.workspace(label)
            .ok_or_else(|| format!("herdr created no workspace labelled '{label}'"))
    }

    fn cos_pane(
        &mut self,
        workspace: &str,
        recorded: Option<&(String, String)>,
    ) -> Result<CosPane, String> {
        let lists = PaneLists {
            panes: self.listing(&["pane", "list"])?,
            tabs: self.listing(&["tab", "list"])?,
            agents: self.listing(&["agent", "list"])?,
        };
        cos_pane(&lists, workspace, recorded)
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
        dialog: bool,
    ) -> Result<(), String> {
        // Never typed into a pane that already runs an agent, whatever herdr would do.
        if self.agent_in_pane(pane).is_some() {
            return Ok(());
        }
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
        // `agent_not_ready` is how a start comes back while the agent IS running (a
        // folder-trust dialog, or readiness past the timeout). Take the same step a
        // worker spawn takes, then believe the pane over the reply.
        if r.error_code().as_deref() == Some("agent_not_ready") {
            if dialog {
                let _ = crate::task::spawn::accept_trust_dialog(
                    &Session { up: self },
                    name,
                    pane,
                    &crate::task::spawn::Ready::DEFAULT,
                );
            }
            if self.agent_in_pane(pane).is_some() {
                return Ok(());
            }
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

/// [`CliUp`] as a [`HerdrClient`], for the one shared step that takes one: the folder-trust
/// acceptance. Only the three verbs that step uses answer; the rest stay unsupported.
struct Session<'a> {
    up: &'a CliUp,
}

impl HerdrClient for Session<'_> {
    fn agent_start(
        &self,
        _name: &str,
        _kind: &str,
        _pane: &str,
        _timeout_ms: u32,
        _agent_args: &[&str],
    ) -> std::io::Result<Reply> {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "agent start goes through UpHost::start_agent",
        ))
    }

    fn agent_get(&self, name: &str) -> std::io::Result<Reply> {
        self.up.herdr(&["agent", "get", name])
    }

    fn agent_list(&self) -> std::io::Result<Reply> {
        self.up.herdr(&["agent", "list"])
    }

    fn agent_wait(&self, name: &str, until: &[&str], timeout_ms: u64) -> std::io::Result<Reply> {
        let mut args = vec!["agent", "wait", name];
        let t = timeout_ms.to_string();
        for s in until {
            args.push("--until");
            args.push(s);
        }
        args.push("--timeout");
        args.push(&t);
        self.up.herdr(&args)
    }

    fn agent_prompt(
        &self,
        _name: &str,
        _text: &str,
        _wait: Option<(&[&str], u64)>,
    ) -> std::io::Result<Reply> {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "agent prompt is not part of quaqua up",
        ))
    }

    fn agent_send_keys(&self, name: &str, keys: &[&str]) -> std::io::Result<Reply> {
        let mut args = vec!["agent", "send-keys", name];
        args.extend(keys.iter().copied());
        self.up.herdr(&args)
    }

    fn pane_read(&self, pane: &str, lines: u32) -> std::io::Result<Reply> {
        let n = lines.to_string();
        self.up.herdr(&[
            "pane",
            "read",
            pane,
            "--source",
            "recent-unwrapped",
            "--lines",
            &n,
        ])
    }
}
