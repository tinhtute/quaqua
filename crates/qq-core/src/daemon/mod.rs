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
use std::fs::{File, OpenOptions, TryLockError};
use std::io::{self, BufReader};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use serde_json::{json, Value};

use crate::herdr::HerdrClient;
use crate::rpc::{self, Request, Response, RpcError};
use store::Store;
use supervisor::{Factory, LiveHook, Supervisor};

/// A live watcher: kept so a repeat of its key is a no-op while it runs.
struct WatcherSlot {
    workflow: String,
    restart_finished: bool,
    handle: JoinHandle<()>,
}

/// What a verb handler can reach.
pub struct Ctx {
    pub runs_dir: PathBuf,
    pub socket: PathBuf,
    pub store: Arc<Store>,
    pub supervisor: Arc<Supervisor>,
    pub herdr: Arc<dyn HerdrClient>,
    shutdown: Arc<AtomicBool>,
    stop_watchers: Arc<AtomicBool>,
    watchers: Mutex<HashMap<String, WatcherSlot>>,
}

impl Ctx {
    /// Starts a watcher under `key`; a repeat of a live key is a no-op, so two
    /// calls never journal one change twice. `restart_finished` marks an agent
    /// watcher, which a later call replaces once it ends; a report watcher
    /// stays until its workflow ends, since its one Signal is already out.
    pub fn add_watcher(
        &self,
        key: &str,
        workflow: &str,
        restart_finished: bool,
        spawn: impl FnOnce(Arc<Supervisor>, Arc<AtomicBool>) -> JoinHandle<()>,
    ) -> Result<(), RpcError> {
        if !self.supervisor.is_running(workflow) {
            return Err(RpcError::new(
                "no_such_workflow",
                format!("workflow '{workflow}' is not running"),
            ));
        }
        let mut watchers = self.watchers.lock().unwrap_or_else(|p| p.into_inner());
        // Prune the finished: an agent watcher ends for good (agent gone or
        // workflow done); a report watcher is kept only while its workflow
        // runs, so a repeat of its key cannot signal the same file twice.
        watchers.retain(|_, w| {
            !w.handle.is_finished()
                || (!w.restart_finished && self.supervisor.is_running(&w.workflow))
        });
        if watchers.contains_key(key) {
            return Ok(());
        }
        let handle = spawn(
            Arc::clone(&self.supervisor),
            Arc::clone(&self.stop_watchers),
        );
        watchers.insert(
            key.to_string(),
            WatcherSlot {
                workflow: workflow.to_string(),
                restart_finished,
                handle,
            },
        );
        Ok(())
    }

    /// Watches `agent` in herdr for `workflow` and remembers the watch, so a
    /// restarted daemon re-arms it. A repeat changes nothing.
    pub fn watch_agent(&self, workflow: &str, agent: &str) -> Result<(), RpcError> {
        self.add_watcher(
            &format!("agent:{workflow}:{agent}"),
            workflow,
            true,
            |sup, stop| {
                watch::watch_agent(
                    Arc::clone(&self.herdr),
                    agent.to_string(),
                    workflow.to_string(),
                    sup,
                    stop,
                )
            },
        )?;
        self.store
            .remember_agent_watch(workflow, agent, now_ms())
            .map_err(internal)
    }

    /// Watches the report file `path` (already normalized) for `workflow` and
    /// remembers the watch, so a restarted daemon re-arms it. A report that
    /// already exists is seen at once; the flow treats `report/appeared` as
    /// idempotent. A repeat changes nothing.
    pub fn watch_report(&self, workflow: &str, path: &Path) -> Result<(), RpcError> {
        self.add_watcher(
            &format!("report:{workflow}:{}", path.display()),
            workflow,
            false,
            |sup, stop| watch::watch_report(path.to_path_buf(), workflow.to_string(), sup, stop),
        )?;
        self.store
            .remember_report_watch(workflow, path, now_ms())
            .map_err(internal)
    }

    /// Re-arms the remembered agent and report watches of every workflow whose
    /// loop runs. A watch whose workflow is finished or gone is skipped.
    ///
    /// Delivery is at-least-once: a re-armed agent watcher reports the current
    /// status first, changed or not, so a restart can repeat the last status.
    fn rearm_watches(&self) -> Result<(), String> {
        for (workflow, agent) in self.store.agent_watches()? {
            if self.supervisor.is_running(&workflow) {
                let _ = self.watch_agent(&workflow, &agent);
            }
        }
        for (workflow, path) in self.store.report_watches()? {
            if self.supervisor.is_running(&workflow) {
                let _ = self.watch_report(&workflow, &path);
            }
        }
        Ok(())
    }

    /// Asks the daemon to stop; wakes the accept loop.
    pub fn request_shutdown(&self) {
        self.shutdown.store(true, Ordering::SeqCst);
        let _ = UnixStream::connect(&self.socket);
    }
}

/// A verb handler gets the whole request, caller included.
pub type Handler = Box<dyn Fn(&Ctx, &Request) -> Result<Value, RpcError> + Send + Sync>;

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
    live_hooks: Vec<LiveHook>,
}

fn str_arg<'a>(args: &'a Value, key: &str) -> Result<&'a str, RpcError> {
    args.get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| RpcError::new(rpc::BAD_REQUEST, format!("missing string argument '{key}'")))
}

fn internal(e: String) -> RpcError {
    RpcError::new("internal", e)
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

impl Daemon {
    pub fn new(cfg: Config) -> Self {
        let mut d = Daemon {
            cfg,
            verbs: BTreeMap::new(),
            kinds: HashMap::new(),
            live_hooks: Vec::new(),
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

    /// Adds a workflow kind; a name that is already taken is refused.
    pub fn register_kind(&mut self, kind: &str, factory: Factory) -> Result<(), String> {
        if self.kinds.contains_key(kind) {
            return Err(format!("workflow kind '{kind}' is already registered"));
        }
        self.kinds.insert(kind.to_string(), factory);
        Ok(())
    }

    /// The herdr handle the daemon was configured with, for installs that
    /// would otherwise rebuild their own.
    pub fn herdr(&self) -> Arc<dyn HerdrClient> {
        Arc::clone(&self.cfg.herdr)
    }

    /// The workspace `runs/` directory the daemon serves.
    pub fn runs_dir(&self) -> &Path {
        &self.cfg.runs_dir
    }

    /// Adds a hook run for each workflow whose loop goes live, on a new start
    /// and on each recovery at daemon start.
    pub fn on_workflow_live(&mut self, hook: LiveHook) {
        self.live_hooks.push(hook);
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
            Box::new(|ctx, req| {
                let args = &req.args;
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
            Box::new(|ctx, req| {
                let args = &req.args;
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
            Box::new(|ctx, req| {
                let args = &req.args;
                let wf = str_arg(args, "workflow")?;
                let agent = str_arg(args, "agent")?;
                ctx.watch_agent(wf, agent)?;
                Ok(json!({ "watching": agent }))
            }),
        );
        add(
            self,
            "watch.report",
            Box::new(|ctx, req| {
                let args = &req.args;
                let wf = str_arg(args, "workflow")?;
                let path = normalize(&ctx.runs_dir, str_arg(args, "path")?);
                ctx.watch_report(wf, &path)?;
                Ok(json!({ "watching": path }))
            }),
        );
    }

    /// Runs until a `stop` verb arrives. Unfinished workflows stay in the
    /// database and resume at the next start.
    pub fn serve(self) -> Result<(), DaemonError> {
        self.run(None)
    }

    /// `ready` is told once the socket is bound and the workflows revived.
    fn run(self, ready: Option<mpsc::Sender<()>>) -> Result<(), DaemonError> {
        let Daemon {
            cfg,
            verbs,
            kinds,
            live_hooks,
        } = self;
        let socket = rpc::socket_path(&cfg.runs_dir);
        std::fs::create_dir_all(&cfg.runs_dir)?;
        let db_path = cfg.runs_dir.join(crate::db::path::DB_FILE);
        // Held to the end of the run: a second daemon on this workspace fails here.
        let lock = lock_workspace(&db_path, &socket)?;
        let store = Arc::new(Store::open(&db_path).map_err(DaemonError::Store)?);
        let listener = bind_socket(&socket)?;
        let supervisor = Supervisor::new(Arc::clone(&store), kinds);
        for h in live_hooks {
            supervisor.on_live(h);
        }
        supervisor.recover().map_err(DaemonError::Store)?;
        let ctx = Arc::new(Ctx {
            runs_dir: cfg.runs_dir,
            socket: socket.clone(),
            store,
            supervisor,
            herdr: cfg.herdr,
            shutdown: Arc::new(AtomicBool::new(false)),
            stop_watchers: Arc::new(AtomicBool::new(false)),
            watchers: Mutex::new(HashMap::new()),
        });
        ctx.rearm_watches().map_err(DaemonError::Store)?;
        let verbs = Arc::new(verbs);
        if let Some(tx) = ready {
            let _ = tx.send(());
        }
        for conn in listener.incoming() {
            if ctx.shutdown.load(Ordering::SeqCst) {
                break;
            }
            let Ok(stream) = conn else { continue };
            let (ctx, verbs) = (Arc::clone(&ctx), Arc::clone(&verbs));
            std::thread::spawn(move || serve_conn(stream, &ctx, &verbs));
        }
        // Stopping: close the door first, so a new daemon is not kept out by
        // a watcher blocked in `agent wait` (herdr cannot cancel it).
        drop(listener);
        let _ = std::fs::remove_file(&socket);
        ctx.stop_watchers.store(true, Ordering::SeqCst);
        ctx.supervisor.shutdown();
        // The loops are quiet: the next daemon may own the database now.
        drop(lock);
        let watchers: Vec<_> = ctx
            .watchers
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .drain()
            .map(|(_, w)| w.handle)
            .collect();
        for w in watchers {
            let _ = w.join();
        }
        Ok(())
    }

    /// Runs [`Daemon::serve`] on a thread and returns once it is listening.
    pub fn spawn(self) -> Result<DaemonHandle, DaemonError> {
        let socket = rpc::socket_path(&self.cfg.runs_dir);
        let (tx, rx) = mpsc::channel();
        let thread = std::thread::spawn(move || self.run(Some(tx)));
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        loop {
            match rx.recv_timeout(Duration::from_millis(10)) {
                Ok(()) => return Ok(DaemonHandle { socket, thread }),
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    return match thread.join() {
                        Ok(Err(e)) => Err(e),
                        _ => Err(DaemonError::Io(io::Error::other("daemon exited at start"))),
                    };
                }
            }
            if std::time::Instant::now() > deadline {
                return Err(DaemonError::Io(io::Error::other("daemon never listened")));
            }
        }
    }
}

/// An exclusive advisory lock on the database file itself, so the one-daemon
/// rule needs no sidecar file. The OS drops it when the process dies.
///
/// A restart overlaps the old daemon's last moments, and a child forked by
/// another thread shares the lock's open file until it execs, so a held lock
/// gets a short grace before it means another daemon is running.
fn lock_workspace(db: &Path, socket: &Path) -> Result<File, DaemonError> {
    let f = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(db)?;
    let deadline = std::time::Instant::now() + LOCK_GRACE;
    loop {
        match f.try_lock() {
            Ok(()) => return Ok(f),
            Err(TryLockError::WouldBlock) if std::time::Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(TryLockError::WouldBlock) => {
                return Err(DaemonError::AlreadyRunning(socket.to_path_buf()))
            }
            Err(TryLockError::Error(e)) => return Err(e.into()),
        }
    }
}

/// How long a held workspace lock is waited for before it counts as a running daemon.
const LOCK_GRACE: Duration = Duration::from_millis(1500);

/// `path` made absolute under `runs_dir` with `.` and `..` folded out, so two
/// spellings of one file share one watcher key. Lexical: touches no disk.
fn normalize(runs_dir: &Path, path: &str) -> PathBuf {
    let mut out = PathBuf::new();
    for c in runs_dir.join(path).components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other),
        }
    }
    out
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
                Some(h) => match h(ctx, &req) {
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
