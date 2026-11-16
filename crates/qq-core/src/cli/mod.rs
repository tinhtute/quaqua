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
        let mut s = String::from("usage: qq <command> [args]\n");
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
                eprintln!("qq: {}", e.message);
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
    crate::bus::INSTALL,
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

/// Entry point of the `qq` binary.
pub fn run() -> i32 {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let env = Env {
        runs_dir: crate::db::path::default_runs_dir(),
    };
    match assemble(&env) {
        Ok((registry, _)) => registry.dispatch(&env, &args),
        Err(e) => {
            eprintln!("qq: {e}");
            1
        }
    }
}

/// Sends one verb to the daemon: a refusal or a missing daemon is an error at once.
pub fn rpc_call(env: &Env, verb: &str, args: Value) -> Result<Value, CliError> {
    let socket = rpc::socket_path(&env.runs_dir);
    let mut req = Request::new(verb, args);
    // Client-asserted: the name comes from the calling agent's own env.
    req.caller = std::env::var("QQ_AGENT").ok().filter(|n| !n.is_empty());
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
                1,
                format!("daemon not running (socket {})", socket.display()),
            ))
        }
        Err(e) => Err(CliError::new(1, format!("rpc failed: {e}"))),
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
            name: "status",
            help: "list workflows from the daemon's database",
            run: |env, _| rpc_call(env, "status", Value::Null).map(|v| pretty(&v)),
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
                        "usage: qq signal <workflow> <name> [json]",
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
                    return Err(CliError::new(2, "usage: qq call <verb> [json]"));
                };
                rpc_call(env, verb, json_arg(a.get(1))?).map(|v| pretty(&v))
            },
        },
    ]
}
