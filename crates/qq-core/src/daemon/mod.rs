//! The farm daemon: one process per workspace, a Unix-socket RPC server in
//! front of a workflow supervisor, watchers that only emit Signals, and one
//! database door. Every state value lives in `runs/qq.db`; the only other
//! file the daemon creates is its socket.
//!
//! Other modules add verbs with [`Daemon::register_verb`] and workflow kinds
//! with [`Daemon::register_kind`]; this file holds only the built-in verbs.

pub mod store;
pub mod supervisor;
pub mod watch;

use std::collections::{BTreeMap, HashMap};
use std::io::{self, BufReader};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use serde_json::{json, Value};

use crate::herdr::HerdrClient;
use crate::rpc::{self, Request, Response, RpcError};
use store::Store;
use supervisor::{Factory, Supervisor};

/// What a verb handler can reach.
pub struct Ctx {
    pub runs_dir: PathBuf,
    pub socket: PathBuf,
    pub store: Arc<Store>,
    pub supervisor: Arc<Supervisor>,
    pub herdr: Arc<dyn HerdrClient>,
    shutdown: Arc<AtomicBool>,
    stop_watchers: Arc<AtomicBool>,
    watchers: Mutex<Vec<JoinHandle<()>>>,
}

impl Ctx {
    /// Asks the daemon to stop; wakes the accept loop.
    pub fn request_shutdown(&self) {
        self.shutdown.store(true, Ordering::SeqCst);
        let _ = UnixStream::connect(&self.socket);
    }
}

pub type Handler = Box<dyn Fn(&Ctx, &Value) -> Result<Value, RpcError> + Send + Sync>;

pub struct Config {
    pub runs_dir: PathBuf,
    pub herdr: Arc<dyn HerdrClient>,
}

#[derive(Debug)]
pub enum DaemonError {
    /// Another daemon already answers on the socket.
    AlreadyRunning(PathBuf),
    Store(String),
    Io(io::Error),
}

impl std::fmt::Display for DaemonError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DaemonError::AlreadyRunning(p) => {
                write!(f, "daemon already running on {}", p.display())
            }
            DaemonError::Store(e) => write!(f, "database: {e}"),
            DaemonError::Io(e) => write!(f, "io: {e}"),
        }
    }
}

impl std::error::Error for DaemonError {}

impl From<io::Error> for DaemonError {
    fn from(e: io::Error) -> Self {
        DaemonError::Io(e)
    }
}

pub struct Daemon {
    cfg: Config,
    verbs: BTreeMap<String, Handler>,
    kinds: HashMap<String, Factory>,
}

fn str_arg<'a>(args: &'a Value, key: &str) -> Result<&'a str, RpcError> {
    args.get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| RpcError::new(rpc::BAD_REQUEST, format!("missing string argument '{key}'")))
}

fn internal(e: String) -> RpcError {
    RpcError::new("internal", e)
}

impl Daemon {
    pub fn new(cfg: Config) -> Self {
        let mut d = Daemon {
            cfg,
            verbs: BTreeMap::new(),
            kinds: HashMap::new(),
        };
        d.register_builtin();
        d
    }

    /// Adds a verb; a name that is already taken is refused.
    pub fn register_verb(&mut self, name: &str, handler: Handler) -> Result<(), String> {
        if self.verbs.contains_key(name) {
            return Err(format!("verb '{name}' is already registered"));
        }
        self.verbs.insert(name.to_string(), handler);
        Ok(())
    }

    pub fn register_kind(&mut self, kind: &str, factory: Factory) {
        self.kinds.insert(kind.to_string(), factory);
    }

    fn register_builtin(&mut self) {
        let add = |d: &mut Daemon, name: &str, h: Handler| {
            d.verbs.insert(name.to_string(), h);
        };
        add(
            self,
            "ping",
            Box::new(|_, _| Ok(json!({ "pong": true, "pid": std::process::id() }))),
        );
        add(
            self,
            "status",
            Box::new(|ctx, _| {
                let rows = ctx.store.workflows().map_err(internal)?;
                Ok(json!({
                    "socket": ctx.socket,
                    "workflows": rows.iter().map(|w| {
                        let mut v = w.to_json();
                        v["loop"] = json!(ctx.supervisor.is_running(&w.id));
                        v
                    }).collect::<Vec<_>>(),
                }))
            }),
        );
        add(
            self,
            "stop",
            Box::new(|ctx, _| {
                ctx.request_shutdown();
                Ok(json!({ "stopping": true }))
            }),
        );
        add(
            self,
            "workflow.start",
            Box::new(|ctx, args| {
                let kind = str_arg(args, "kind")?;
                let id = str_arg(args, "id")?;
                let input = args.get("input").cloned().unwrap_or(Value::Null);
                let created = ctx
                    .supervisor
                    .start(kind, id, &input)
                    .map_err(|e| RpcError::new("workflow_start_refused", e))?;
                Ok(json!({ "id": id, "created": created }))
            }),
        );
        add(
            self,
            "signal",
            Box::new(|ctx, args| {
                let id = str_arg(args, "workflow")?;
                let name = str_arg(args, "name")?;
                let source = args.get("source").and_then(Value::as_str).unwrap_or("rpc");
                let payload = args.get("payload").cloned().unwrap_or(Value::Null);
                ctx.supervisor
                    .signal(id, source, name, payload)
                    .map_err(|e| RpcError::new("no_such_workflow", e))?;
                Ok(json!({ "delivered": true }))
            }),
        );
        add(
            self,
            "watch.agent",
            Box::new(|ctx, args| {
                let wf = str_arg(args, "workflow")?;
                let agent = str_arg(args, "agent")?;
                if !ctx.supervisor.is_running(wf) {
                    return Err(RpcError::new(
                        "no_such_workflow",
                        format!("workflow '{wf}' is not running"),
                    ));
                }
                let h = watch::watch_agent(
                    Arc::clone(&ctx.herdr),
                    agent.to_string(),
                    wf.to_string(),
                    Arc::clone(&ctx.supervisor),
                    Arc::clone(&ctx.stop_watchers),
                );
                ctx.watchers
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .push(h);
                Ok(json!({ "watching": agent }))
            }),
        );
        add(
            self,
            "watch.report",
            Box::new(|ctx, args| {
                let wf = str_arg(args, "workflow")?;
                let path = str_arg(args, "path")?;
                if !ctx.supervisor.is_running(wf) {
                    return Err(RpcError::new(
                        "no_such_workflow",
                        format!("workflow '{wf}' is not running"),
                    ));
                }
                let h = watch::watch_report(
                    PathBuf::from(path),
                    wf.to_string(),
                    Arc::clone(&ctx.supervisor),
                    Arc::clone(&ctx.stop_watchers),
                );
                ctx.watchers
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .push(h);
                Ok(json!({ "watching": path }))
            }),
        );
    }

    /// Runs until a `stop` verb arrives. Unfinished workflows stay in the
    /// database and resume at the next start.
    pub fn serve(self) -> Result<(), DaemonError> {
        let Daemon { cfg, verbs, kinds } = self;
        let store = Arc::new(
            Store::open(&cfg.runs_dir.join(crate::db::path::DB_FILE))
                .map_err(DaemonError::Store)?,
        );
        let socket = rpc::socket_path(&cfg.runs_dir);
        let listener = bind_socket(&socket)?;
        let supervisor = Supervisor::new(Arc::clone(&store), kinds);
        supervisor.recover().map_err(DaemonError::Store)?;
        let ctx = Arc::new(Ctx {
            runs_dir: cfg.runs_dir,
            socket: socket.clone(),
            store,
            supervisor,
            herdr: cfg.herdr,
            shutdown: Arc::new(AtomicBool::new(false)),
            stop_watchers: Arc::new(AtomicBool::new(false)),
            watchers: Mutex::new(Vec::new()),
        });
        let verbs = Arc::new(verbs);
        for conn in listener.incoming() {
            if ctx.shutdown.load(Ordering::SeqCst) {
                break;
            }
            let Ok(stream) = conn else { continue };
            let (ctx, verbs) = (Arc::clone(&ctx), Arc::clone(&verbs));
            std::thread::spawn(move || serve_conn(stream, &ctx, &verbs));
        }
        ctx.stop_watchers.store(true, Ordering::SeqCst);
        ctx.supervisor.shutdown();
        let watchers: Vec<_> = ctx
            .watchers
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .drain(..)
            .collect();
        for w in watchers {
            let _ = w.join();
        }
        let _ = std::fs::remove_file(&socket);
        Ok(())
    }

    /// Runs [`Daemon::serve`] on a thread and returns once the socket answers.
    pub fn spawn(self) -> Result<DaemonHandle, DaemonError> {
        let socket = rpc::socket_path(&self.cfg.runs_dir);
        if UnixStream::connect(&socket).is_ok() {
            return Err(DaemonError::AlreadyRunning(socket));
        }
        let thread = std::thread::spawn(move || self.serve());
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        loop {
            if thread.is_finished() {
                return match thread.join() {
                    Ok(Err(e)) => Err(e),
                    _ => Err(DaemonError::Io(io::Error::other("daemon exited at start"))),
                };
            }
            if UnixStream::connect(&socket).is_ok() {
                return Ok(DaemonHandle { socket, thread });
            }
            if std::time::Instant::now() > deadline {
                return Err(DaemonError::Io(io::Error::other("daemon never listened")));
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

pub struct DaemonHandle {
    pub socket: PathBuf,
    thread: JoinHandle<Result<(), DaemonError>>,
}

impl DaemonHandle {
    /// Sends `stop` and waits for the daemon to wind down.
    pub fn stop(self) -> Result<(), DaemonError> {
        let _ = rpc::call(&self.socket, &Request::new("stop", Value::Null));
        self.thread
            .join()
            .unwrap_or_else(|_| Err(DaemonError::Io(io::Error::other("daemon panicked"))))
    }
}

/// Binds `path`. A leftover socket whose owner is gone (connect refused) is
/// replaced; one that still answers means a daemon is running.
fn bind_socket(path: &Path) -> Result<UnixListener, DaemonError> {
    match UnixListener::bind(path) {
        Ok(l) => Ok(l),
        Err(e) if e.kind() == io::ErrorKind::AddrInUse => {
            if UnixStream::connect(path).is_ok() {
                return Err(DaemonError::AlreadyRunning(path.to_path_buf()));
            }
            std::fs::remove_file(path)?;
            Ok(UnixListener::bind(path)?)
        }
        Err(e) => Err(e.into()),
    }
}

fn serve_conn(stream: UnixStream, ctx: &Ctx, verbs: &BTreeMap<String, Handler>) {
    let Ok(mut out) = stream.try_clone() else {
        return;
    };
    let mut reader = BufReader::new(stream);
    while let Ok(Some(line)) = rpc::read_line(&mut reader) {
        if line.trim().is_empty() {
            continue;
        }
        let resp = match serde_json::from_str::<Request>(&line) {
            Err(e) => Response::err(RpcError::new(rpc::BAD_REQUEST, e.to_string())),
            Ok(req) => match verbs.get(&req.verb) {
                None => Response::err(RpcError::new(
                    rpc::UNKNOWN_VERB,
                    format!("no verb '{}'", req.verb),
                )),
                Some(h) => match h(ctx, &req.args) {
                    Ok(v) => Response::ok(v),
                    Err(e) => Response::err(e),
                },
            },
        };
        if rpc::write_line(&mut out, &resp).is_err() {
            return;
        }
    }
}
