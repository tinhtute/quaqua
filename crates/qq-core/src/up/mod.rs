//! `quaqua up` (R1-R5): the one command that brings the CoS up — the herdr session,
//! the farm daemon, the workspace, the CoS pane — and hands the terminal to herdr.
//!
//! Order, and why: the herdr session first (everything else needs it, and a human
//! whose `roles.cos.*` keys are missing must be able to reach a herdr pane to run
//! `config set`), then the daemon ([`daemon::ensure_answers`]), then the CoS launch
//! from config ([`launch_from`], whose message `up` prints verbatim), then the
//! workspace and the CoS pane ([`herdr`]), then focus, then the TUI when the caller
//! is outside herdr.

pub mod daemon;
pub mod herdr;

use crate::cli::{CliError, Command, Env, Install, Registry, EXIT_UNREACHABLE};
use crate::config::{self, CosLaunch};
use crate::daemon::Daemon;
use crate::identity;

pub use herdr::{Agent, CliUp, UpHost, Workspace};

/// How the CoS was brought up, for the tests and the lines.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cos {
    /// It was already running: nothing was started.
    Live,
    /// It had exited and its recorded session was resumed.
    Resumed,
    /// A fresh session was started: a new workspace, no record, or a failed resume.
    Fresh,
}

/// The `[roles.cos]` launch, with the error already shaped for the CLI: the message is
/// `cos_launch`'s own, verbatim, at exit 1 (R2).
pub fn launch_from(conn: &rusqlite::Connection) -> Result<CosLaunch, CliError> {
    config::cos_launch(conn).map_err(|e| CliError::new(1, e.to_string()))
}

/// Brings the CoS up. `Ok` is a farm a human can use; `Err` carries the exit status and
/// the line to show (the daemon half's own [`crate::cli::CliError`], or `cos_launch`'s
/// message untouched).
pub fn up(
    host: &mut dyn UpHost,
    daemon: &mut dyn crate::up::daemon::DaemonHost,
) -> Result<Cos, CliError> {
    // R1: the herdr session, before anything that needs herdr.
    if !host.session_running() {
        host.start_session()
            .map_err(|e| CliError::new(EXIT_UNREACHABLE, e))?;
        host.line(&format!(
            "quaqua up: started the herdr session '{}'",
            host.name()
        ));
    }
    // R4: the daemon answers, or up says why it cannot.
    daemon::ensure_answers(daemon)?;
    // R2: the CoS launch from `[roles.cos]`, verbatim.
    let launch = host.launch()?;
    // R1: the workspace labelled after the farm, its cwd the farm root.
    let root = host.workspace_root();
    let label = identity::farm_of(&root);
    let (ws, created) = match host.workspace(&label) {
        Some(ws) => {
            host.line(&format!(
                "quaqua up: the workspace '{label}' is there ({}), its CoS pane {}",
                ws.id, ws.root_pane
            ));
            (ws, false)
        }
        None => {
            let ws = host.create_workspace(&label, &root).map_err(|e| {
                CliError::new(EXIT_UNREACHABLE, format!("herdr workspace '{label}': {e}"))
            })?;
            host.line(&format!(
                "quaqua up: created the workspace '{label}' at {} (pane {})",
                root.display(),
                ws.root_pane
            ));
            (ws, true)
        }
    };
    // R3: the CoS in the workspace's root pane.
    let cos = ensure_cos(host, &launch, &ws.root_pane, created)?;
    host.focus(config::COS_ROLE)
        .map_err(|e| CliError::new(1, format!("herdr agent focus {}: {e}", config::COS_ROLE)))?;
    // R1: the TUI only outside herdr; inside, focus and return.
    if !host.inside_herdr() {
        host.attach()
            .map_err(|e| CliError::new(1, format!("herdr session attach: {e}")))?;
    }
    Ok(cos)
}

/// The CoS: reuse a live one, resume a recorded one, else start fresh. Never leaves the
/// farm without a CoS: a failed resume is one line naming the id, then a fresh start.
fn ensure_cos(
    host: &mut dyn UpHost,
    launch: &CosLaunch,
    pane: &str,
    created: bool,
) -> Result<Cos, CliError> {
    let name = config::COS_ROLE;
    if let Some(agent) = host.agent(name) {
        host.line(&format!(
            "quaqua up: the CoS is running in pane {} ({})",
            agent.pane, launch.harness
        ));
        return Ok(Cos::Live);
    }
    // A workspace `up` just made never ran a CoS, so a stale record cannot be resumed.
    let recorded = if created {
        None
    } else {
        host.recorded_session()
    };
    if let Some(session) = recorded {
        match resume_args(&launch.harness, &session) {
            Some(more) => {
                let mut args = launch.agent_args().to_vec();
                args.extend(more);
                match host.start_agent(name, &launch.herdr_kind, pane, &args) {
                    Ok(()) => {
                        host.line(&format!(
                            "quaqua up: resumed the CoS session {session} in pane {pane}"
                        ));
                        return Ok(Cos::Resumed);
                    }
                    Err(e) => host.line(&format!(
                        "quaqua up: could not resume the CoS session {session} ({e}); starting a fresh one"
                    )),
                }
            }
            // No verified flag for this harness: never append a guessed one.
            None => host.line(&format!(
                "quaqua up: {} has no resume flag; dropping the recorded session {session} and starting fresh",
                launch.harness
            )),
        }
    } else if created {
        // The new workspace's root pane is the announcement.
    } else {
        host.line("quaqua up: no recorded session; starting a fresh CoS");
    }
    start_fresh(host, launch, pane)
}

fn start_fresh(host: &mut dyn UpHost, launch: &CosLaunch, pane: &str) -> Result<Cos, CliError> {
    host.start_agent(
        config::COS_ROLE,
        &launch.herdr_kind,
        pane,
        launch.agent_args(),
    )
    .map_err(|e| {
        CliError::new(
            1,
            format!(
                "herdr agent start {} --kind {} --pane {pane} failed: {e}",
                config::COS_ROLE,
                launch.herdr_kind
            ),
        )
    })?;
    host.line(&format!(
        "quaqua up: started a fresh CoS in pane {pane} ({}, {} {})",
        launch.harness, launch.model, launch.effort
    ));
    Ok(Cos::Fresh)
}

/// The harness's resume flag and the recorded id, from the driver that owns it; `None`
/// when the driver has no verified flag (a fresh session is then the only move).
fn resume_args(harness: &str, session: &str) -> Option<Vec<String>> {
    crate::driver::Registry::native()
        .get(harness)?
        .resume_args(session)
}

/// The entry `cli::MODULES` lists (R5).
pub const INSTALL: Install = install;

const USAGE: &str = "usage: quaqua up";

fn install(registry: &mut Registry, _daemon: &mut Daemon) -> Result<(), String> {
    registry.register(Command {
        name: "up",
        help: "bring the CoS up: herdr session, daemon, workspace, CoS pane",
        run: up_cmd,
    })
}

fn up_cmd(env: &Env, args: &[String]) -> Result<String, CliError> {
    if !args.is_empty() {
        return Err(CliError::new(crate::cli::EXIT_USAGE, USAGE));
    }
    let mut host = CliUp::new(env.runs_dir.clone());
    let mut sys = daemon::SystemdHost::new(env.runs_dir.clone());
    up(&mut host, &mut sys).map(|_| String::new())
}
