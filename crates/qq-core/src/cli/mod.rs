//! The `qq` command line: a client of the daemon plus the `daemon` verb that
//! runs it. A module adds its commands and daemon verbs with one `install`
//! function and one line in [`MODULES`]; the dispatch below never changes.

use std::collections::BTreeMap;
use std::io;
use std::path::PathBuf;
use std::sync::Arc;

use serde_json::Value;

use crate::daemon::{Config, Daemon};
use crate::herdr::CliHerdr;
use crate::rpc::{self, Request};

/// What a command runs against.
pub struct Env {
    pub runs_dir: PathBuf,
}

/// Exit status for a usage error: nothing ran, so sending the same thing again
/// cannot help (sysexits EX_USAGE).
pub const EXIT_USAGE: i32 = 64;
/// Exit status when the daemon cannot be reached (sysexits EX_UNAVAILABLE).
/// Start the daemon; do not resend. Exit 1 keeps its per-command meaning.
pub const EXIT_UNREACHABLE: i32 = 69;

/// A failed command: the message goes to stderr, `code` is the exit status.
#[derive(Debug)]
pub struct CliError {
    pub code: i32,
    pub message: String,
}

impl CliError {
    pub fn new(code: i32, message: impl Into<String>) -> Self {
        CliError {
            code,
            message: message.into(),
        }
    }
}

pub type Run = fn(&Env, &[String]) -> Result<String, CliError>;

pub struct Command {
    pub name: &'static str,
    pub help: &'static str,
    pub run: Run,
}

#[derive(Default)]
pub struct Registry {
    commands: BTreeMap<&'static str, Command>,
}

impl Registry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a command; a name that is already taken is refused.
    pub fn register(&mut self, c: Command) -> Result<(), String> {
        if self.commands.contains_key(c.name) {
            return Err(format!("command '{}' is already registered", c.name));
        }
        self.commands.insert(c.name, c);
        Ok(())
    }

    fn usage(&self) -> String {
        let mut s = String::from("usage: quaqua <command> [args]\n");
        for c in self.commands.values() {
            s.push_str(&format!("  {:<8} {}\n", c.name, c.help));
        }
        s
    }

    /// Runs `args` (without the program name); returns the exit status.
    pub fn dispatch(&self, env: &Env, args: &[String]) -> i32 {
        let Some(cmd) = args.first().and_then(|n| self.commands.get(n.as_str())) else {
            eprint!("{}", self.usage());
            return 2;
        };
        match (cmd.run)(env, &args[1..]) {
            Ok(out) => {
                if !out.is_empty() {
                    println!("{out}");
                }
                0
            }
            Err(e) => {
                eprintln!("quaqua: {}", e.message);
                e.code
            }
        }
    }
}

/// A module's seam: registers its CLI commands and its daemon verbs and kinds.
pub type Install = fn(&mut Registry, &mut Daemon) -> Result<(), String>;

/// Every module's install. Each module adds exactly one line here.
pub const MODULES: &[Install] = &[
    crate::task::worker_flow::INSTALL,
    crate::task::create::INSTALL,
    crate::bus::INSTALL,
    crate::config::INSTALL,
    crate::authority::INSTALL,
    crate::task::pipeline_io::INSTALL,
    crate::task::prompt::INSTALL,
    crate::grid::INSTALL,
    crate::status::INSTALL,
    crate::playbook::INSTALL,
    crate::memory::INSTALL,
    crate::transcripts::INSTALL,
    crate::identity::INSTALL,
    crate::init::INSTALL,
    crate::auditor::INSTALL,
    crate::run::INSTALL,
    crate::skills::INSTALL,
    crate::kb::INSTALL,
    crate::backlog::INSTALL,
    crate::source::INSTALL,
    crate::supervisor::INSTALL,
];

/// The CLI registry and the daemon, both filled from the core and [`MODULES`].
pub fn assemble(env: &Env) -> Result<(Registry, Daemon), String> {
    let mut registry = Registry::new();
    for c in core_commands() {
        registry.register(c)?;
    }
    let mut daemon = Daemon::new(Config {
        runs_dir: env.runs_dir.clone(),
        herdr: Arc::new(CliHerdr::new()),
    });
    for install in MODULES {
        install(&mut registry, &mut daemon)?;
    }
    Ok((registry, daemon))
}

/// Entry point of the `quaqua` binary.
pub fn run() -> i32 {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if matches!(args.first().map(String::as_str), Some("--version" | "-V")) {
        println!("quaqua {}", env!("CARGO_PKG_VERSION"));
        return 0;
    }
    let env = Env {
        runs_dir: crate::db::path::default_runs_dir(),
    };
    match assemble(&env) {
        Ok((registry, _)) => registry.dispatch(&env, &args),
        Err(e) => {
            eprintln!("quaqua: {e}");
            1
        }
    }
}

/// The `QQ_AGENT` claim of this process, `None` when unset or empty. The one
/// place the env is read; a command reads it once and passes the value down.
pub fn caller_claim() -> Option<String> {
    std::env::var("QQ_AGENT").ok().filter(|n| !n.is_empty())
}

/// Sends one verb to the daemon: a refusal or a missing daemon is an error at once.
pub fn rpc_call(env: &Env, verb: &str, args: Value) -> Result<Value, CliError> {
    rpc_call_as(env, caller_claim(), verb, args)
}

/// [`rpc_call`] with the caller's claim already read: the daemon still names
/// the caller from the socket peer, so the claim is a hint, never authority.
pub fn rpc_call_as(
    env: &Env,
    claim: Option<String>,
    verb: &str,
    args: Value,
) -> Result<Value, CliError> {
    let socket = rpc::socket_path(&env.runs_dir);
    let mut req = Request::new(verb, args);
    req.caller = claim;
    match rpc::call(&socket, &req) {
        Ok(resp) => resp
            .into_result()
            .map_err(|e| CliError::new(1, e.to_string())),
        Err(e)
            if matches!(
                e.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
            ) =>
        {
            Err(CliError::new(
                EXIT_UNREACHABLE,
                format!("daemon not running (socket {})", socket.display()),
            ))
        }
        Err(e) => Err(CliError::new(EXIT_UNREACHABLE, format!("rpc failed: {e}"))),
    }
}

fn pretty(v: &Value) -> String {
    serde_json::to_string_pretty(v).unwrap_or_default()
}

fn json_arg(s: Option<&String>) -> Result<Value, CliError> {
    match s {
        None => Ok(Value::Null),
        Some(raw) => serde_json::from_str(raw)
            .map_err(|e| CliError::new(2, format!("argument is not JSON: {e}"))),
    }
}

fn core_commands() -> Vec<Command> {
    vec![
        Command {
            name: "daemon",
            help: "run the farm daemon in the foreground",
            run: |env, _| {
                let (_, daemon) = assemble(env).map_err(|e| CliError::new(1, e))?;
                daemon
                    .serve()
                    .map(|_| String::new())
                    .map_err(|e| CliError::new(1, e.to_string()))
            },
        },
        Command {
            name: "ping",
            help: "check that the daemon answers",
            run: |env, _| rpc_call(env, "ping", Value::Null).map(|v| pretty(&v)),
        },
        Command {
            name: "stop",
            help: "stop the daemon; workflows resume at the next start",
            run: |env, _| rpc_call(env, "stop", Value::Null).map(|v| pretty(&v)),
        },
        Command {
            name: "signal",
            help: "signal <workflow> <name> [json-payload]",
            run: |env, a| {
                let (Some(wf), Some(name)) = (a.first(), a.get(1)) else {
                    return Err(CliError::new(
                        2,
                        "usage: quaqua signal <workflow> <name> [json]",
                    ));
                };
                let args = serde_json::json!({
                    "workflow": wf, "name": name, "source": "cli",
                    "payload": json_arg(a.get(2))?,
                });
                rpc_call(env, "signal", args).map(|v| pretty(&v))
            },
        },
        Command {
            name: "call",
            help: "call <verb> [json-args]: send any daemon verb",
            run: |env, a| {
                let Some(verb) = a.first() else {
                    return Err(CliError::new(2, "usage: quaqua call <verb> [json]"));
                };
                rpc_call(env, verb, json_arg(a.get(1))?).map(|v| pretty(&v))
            },
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn runs(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("qq-cli-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn missing_socket_is_unreachable() {
        let env = Env { runs_dir: runs("nosock") };
        let e = rpc_call_as(&env, None, "ping", Value::Null).unwrap_err();
        assert_eq!(e.code, EXIT_UNREACHABLE);
        assert!(e.message.starts_with("daemon not running"), "{}", e.message);
    }

    #[test]
    fn live_daemon_that_drops_the_call_is_not_unreachable() {
        let env = Env { runs_dir: runs("eof") };
        let l = std::os::unix::net::UnixListener::bind(rpc::socket_path(&env.runs_dir)).unwrap();
        let h = std::thread::spawn(move || drop(l.accept().unwrap()));
        let e = rpc_call_as(&env, None, "ping", Value::Null).unwrap_err();
        h.join().unwrap();
        assert_eq!(e.code, 1, "{}", e.message);
        assert!(e.message.starts_with("rpc failed:"), "{}", e.message);
        assert!(!e.message.contains("start the daemon"), "{}", e.message);
    }
}
