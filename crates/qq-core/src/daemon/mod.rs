//! The farm daemon: one process per workspace, a Unix-socket RPC server in
//! front of a workflow supervisor, watchers that only emit Signals, and one
//! database door. Every state value lives in `runs/qq.db`; the only other
//! file the daemon creates is its socket.
//!
//! Other modules add verbs with [`Daemon::register_verb`] and workflow kinds
//! with [`Daemon::register_kind`]; this file holds only the built-in verbs.

pub mod caller;
pub mod grant;
pub mod memory_write;
pub mod pr_merge;
pub mod push_door;
pub mod schedule;
pub mod service;
pub mod store;
pub mod supervisor;
pub mod watch;

use std::collections::{BTreeMap, HashMap};
use std::fs::{File, TryLockError};
use std::io::{self, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::thread::JoinHandle;
use std::time::Duration;

use serde_json::{json, Value};

use crate::herdr::HerdrClient;
use crate::identity::peer::{self, PeerResolver};
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
    peers: Option<Arc<dyn PeerResolver>>,
    /// Filled by `run` once the `Ctx` exists; see [`Daemon::report_watcher`].
    link: Arc<OnceLock<Weak<Ctx>>>,
    /// Set only by the CLI daemon command: test daemons stay inert.
    service: bool,
}

/// Registers a report watcher in the daemon's own process: `(workflow, path)`.
pub type ReportWatcher = Arc<dyn Fn(&str, &str) -> Result<(), String> + Send + Sync>;

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
    /// Turns on the service thread (sleep inhibitor, wake mail). Off by default.
    pub fn enable_service(&mut self) {
        self.service = true;
    }

    /// Whether `run` will start the service thread.
    pub fn service_enabled(&self) -> bool {
        self.service
    }

    pub fn new(cfg: Config) -> Self {
        let mut d = Daemon {
            cfg,
            verbs: BTreeMap::new(),
            kinds: HashMap::new(),
            live_hooks: Vec::new(),
            peers: None,
            link: Arc::new(OnceLock::new()),
            service: false,
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

    /// Names every caller from the socket peer: a request's own `caller` is
    /// replaced by the resolved agent, and a peer that maps to none is refused
    /// before any verb runs. Without it the claimed `caller` stands.
    pub fn require_peer(&mut self, peers: Arc<dyn PeerResolver>) {
        self.peers = Some(peers);
    }

    /// The herdr handle the daemon was configured with, for installs that
    /// would otherwise rebuild their own.
    pub fn herdr(&self) -> Arc<dyn HerdrClient> {
        Arc::clone(&self.cfg.herdr)
    }

    /// A report watcher that runs in this daemon's own process. A flow must use
    /// it, never a call to its own socket: the daemon is not in a pane, so the
    /// peer gate refuses it as `unmapped_peer`. Until the daemon is serving
    /// (workflows are revived first) it answers `Err`, and callers retry.
    pub fn report_watcher(&self) -> ReportWatcher {
        let (link, runs_dir) = (Arc::clone(&self.link), self.cfg.runs_dir.clone());
        Arc::new(move |workflow, path| {
            let ctx = link
                .get()
                .and_then(Weak::upgrade)
                .ok_or("daemon is not serving yet")?;
            ctx.watch_report(workflow, &normalize(&runs_dir, path))
                .map_err(|e| e.to_string())
        })
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
        caller::register(self);
        pr_merge::register(self);
        push_door::register(self);
        grant::register(self);
        memory_write::register(self);
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
            peers,
            link,
            service: service_on,
        } = self;
        let socket = rpc::socket_path(&cfg.runs_dir);
        std::fs::create_dir_all(&cfg.runs_dir)?;
        let db_path = cfg.runs_dir.join(crate::db::path::DB_FILE);
        // Held to the end of the run: a second daemon on this workspace fails here.
        let lock = lock_workspace(&cfg.runs_dir, &socket)?;
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
        let _ = link.set(Arc::downgrade(&ctx));
        ctx.rearm_watches().map_err(DaemonError::Store)?;
        watch::state_sweeper(Arc::downgrade(&ctx), Arc::clone(&ctx.stop_watchers));
        schedule::spawn_runner(Arc::downgrade(&ctx), Arc::clone(&ctx.stop_watchers));
        let service = service::spawn(
            service_on,
            Arc::downgrade(&ctx),
            Arc::clone(&ctx.stop_watchers),
        );
        let verbs = Arc::new(verbs);
        if let Some(tx) = ready {
            let _ = tx.send(());
        }
        for conn in listener.incoming() {
            if ctx.shutdown.load(Ordering::SeqCst) {
                break;
            }
            let Ok(stream) = conn else { continue };
            let (ctx, verbs, peers) = (Arc::clone(&ctx), Arc::clone(&verbs), peers.clone());
            std::thread::spawn(move || serve_conn(stream, &ctx, &verbs, peers.as_deref()));
        }
        // Stopping: close the door first, so a new daemon is not kept out by
        // a watcher blocked in `agent wait` (herdr cannot cancel it).
        drop(listener);
        let _ = std::fs::remove_file(&socket);
        ctx.stop_watchers.store(true, Ordering::SeqCst);
        ctx.supervisor.shutdown();
        if let Some(s) = service {
            let _ = s.join();
        }
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
        let link = Arc::clone(&self.link);
        let thread = std::thread::spawn(move || self.run(Some(tx)));
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        loop {
            match rx.recv_timeout(Duration::from_millis(10)) {
                Ok(()) => {
                    return Ok(DaemonHandle {
                        socket,
                        thread,
                        link,
                    })
                }
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

/// The held lock. Dropping unlocks, not just closes: a child forked mid-run
/// shares the open file until it execs, and a plain close would leave the flock
/// with it. Every exit path of `run` goes through this drop.
struct WorkspaceLock(File);

impl Drop for WorkspaceLock {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

/// An exclusive advisory lock on the runs directory, so the one-daemon rule
/// needs no sidecar file. Not on `qq.db`: on macOS a flock and SQLite's fcntl
/// locks on one file conflict, and the daemon could not open its own database.
/// The OS drops it when the process dies.
fn lock_workspace(runs_dir: &Path, socket: &Path) -> Result<WorkspaceLock, DaemonError> {
    let f = File::open(runs_dir)?;
    match f.try_lock() {
        Ok(()) => Ok(WorkspaceLock(f)),
        Err(TryLockError::WouldBlock) => Err(DaemonError::AlreadyRunning(socket.to_path_buf())),
        Err(TryLockError::Error(e)) => Err(e.into()),
    }
}

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
    link: Arc<OnceLock<Weak<Ctx>>>,
}

impl DaemonHandle {
    /// Stops the daemon in-process (no socket call: the owner of the handle
    /// is not a pane peer) and waits for it to wind down.
    pub fn stop(self) -> Result<(), DaemonError> {
        if let Some(ctx) = self.link.get().and_then(Weak::upgrade) {
            ctx.request_shutdown();
        }
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

fn serve_conn(
    stream: UnixStream,
    ctx: &Ctx,
    verbs: &BTreeMap<String, Handler>,
    peers: Option<&dyn PeerResolver>,
) {
    let Ok(mut out) = stream.try_clone() else {
        return;
    };
    // Read once: the peer of a connected socket never changes.
    let cred = peers.map(|_| peer::peer_cred(&stream));
    let own_uid = peer::own_uid();
    let mut reader = BufReader::new(stream);
    while let Ok(Some(line)) = rpc::read_line(&mut reader) {
        if line.trim().is_empty() {
            continue;
        }
        let resp = match serde_json::from_str::<Request>(&line) {
            Err(e) => Response::err(RpcError::new(rpc::BAD_REQUEST, e.to_string())),
            Ok(req) => match identify(req, peers, cred.as_ref(), own_uid) {
                Err(e) => Response::err(e),
                Ok(req) => traced(&req, &mut std::io::stderr(), || match verbs.get(&req.verb) {
                    None => Err(RpcError::new(
                        rpc::UNKNOWN_VERB,
                        format!("no verb '{}'", req.verb),
                    )),
                    Some(h) => h(ctx, &req),
                })
                .map_or_else(Response::err, Response::ok),
            },
        };
        if rpc::write_line(&mut out, &resp).is_err() {
            return;
        }
    }
}

/// Read-only verbs that clients poll (pipeline --wait calls `status` every
/// 500 ms): they log only an err or a call that took `SLOW_POLL_MS` or more.
const POLLING_VERBS: [&str; 4] = ["status", "ping", caller::VERB, "mail.inbox"];
const SLOW_POLL_MS: u128 = 1000;

/// Runs one verb and writes a "start" line before it and a result line after
/// it (caller, verb, duration in ms, ok/err), so a hung verb is visible while
/// it runs and a failed one leaves a trace. Polling verbs write one line, only
/// on err or a slow call (no start line). A write error is ignored.
fn traced<F>(req: &Request, log: &mut dyn Write, run: F) -> Result<Value, RpcError>
where
    F: FnOnce() -> Result<Value, RpcError>,
{
    traced_with(req, log, SLOW_POLL_MS, run)
}

/// `traced` with the slow-poll threshold as a parameter (a test seam).
fn traced_with<F>(
    req: &Request,
    log: &mut dyn Write,
    slow_ms: u128,
    run: F,
) -> Result<Value, RpcError>
where
    F: FnOnce() -> Result<Value, RpcError>,
{
    let polling = POLLING_VERBS.contains(&req.verb.as_str());
    // The claim is unverified without a resolver: {:?} keeps a newline in it
    // from forging a log line.
    let who = req.caller.as_deref().unwrap_or("-");
    if !polling {
        let _ = writeln!(log, "quaqua daemon: verb start caller={who:?} verb={:?}", req.verb);
    }
    let t0 = std::time::Instant::now();
    let res = run();
    let ms = t0.elapsed().as_millis();
    if !polling || res.is_err() || ms >= slow_ms {
        let outcome = if res.is_ok() { "ok" } else { "err" };
        let _ = writeln!(
            log,
            "quaqua daemon: verb caller={who:?} verb={:?} ms={ms} {outcome}",
            req.verb
        );
    }
    res
}

/// With a resolver, `req.caller` becomes the agent the socket peer maps to (the
/// client's claim is dropped). A peer of another uid is refused outright; a peer
/// in a pane with no agent is refused too, whatever the verb. A same-uid peer in
/// no pane at all speaks as the Principal, which may run only [`PRINCIPAL_VERBS`].
fn identify(
    mut req: Request,
    peers: Option<&dyn PeerResolver>,
    cred: Option<&io::Result<peer::PeerCred>>,
    own_uid: u32,
) -> Result<Request, RpcError> {
    let (Some(peers), Some(cred)) = (peers, cred) else {
        return Ok(req);
    };
    let cred = cred.as_ref().map_err(|e| {
        RpcError::new(
            peer::UNMAPPED_PEER,
            format!("caller not identified: socket peer unreadable: {e}"),
        )
    })?;
    if cred.uid != own_uid {
        return Err(RpcError::new(
            peer::FOREIGN_UID,
            format!("peer uid {} is not the daemon's uid {own_uid}", cred.uid),
        ));
    }
    let (who, pane) = peers.resolve_pane(*cred)?;
    let principal = pane.is_none() && who.as_str() == peer::PRINCIPAL;
    if principal && !PRINCIPAL_VERBS.contains(&req.verb.as_str()) {
        return Err(RpcError::new(
            peer::AGENT_ONLY,
            format!(
                "verb '{}' needs an agent: caller pid {} is in no herdr pane, so it speaks as the Principal '{}'",
                req.verb,
                cred.pid,
                peer::PRINCIPAL
            ),
        ));
    }
    req.caller = Some(who.0);
    req.caller_pane = pane;
    Ok(req)
}

/// Verbs a caller in no herdr pane may run: the Principal owns the farm and
/// must see it, stop it and write the config tables from a plain terminal. The
/// list is an allowlist, so a verb added later is closed to the Principal until
/// someone decides otherwise; none of these names an agent. `caller.lookup` is
/// here because its handler is the one that refuses a caller it cannot name as
/// an agent: the Principal reaches it and gets the same `unmapped_peer` answer
/// the hook gate has always read.
const PRINCIPAL_VERBS: [&str; 6] = ["ping", "status", "stop", "config", "roster", caller::VERB];

/// True when the daemon's peer gate named this request's caller as the
/// Principal: a same-uid peer that reaches no herdr pane. The gate overwrites
/// every request's `caller` and `caller_pane` when a resolver is installed, so a
/// client's own claim cannot reach it, and a pane-hosted agent named `owner` is
/// never it either (it has a pane). A daemon with no resolver trusts every
/// claim anyway ([`Daemon::require_peer`]), so this adds no hole there.
pub fn is_principal(req: &Request) -> bool {
    req.caller.as_deref() == Some(peer::PRINCIPAL) && req.caller_pane.is_none()
}

#[cfg(test)]
mod peer_tests {
    use super::*;

    #[test]
    fn default_daemon_spawns_no_service() {
        let cfg = Config {
            runs_dir: std::env::temp_dir().join("qq-svc-flag"),
            herdr: Arc::new(crate::herdr::CliHerdr::new()),
        };
        let mut d = Daemon::new(cfg);
        assert!(!d.service_enabled());
        d.enable_service();
        assert!(d.service_enabled());
    }

    use crate::identity::peer::{CallerId, PeerCred, AGENT_ONLY, PRINCIPAL};

    #[test]
    fn a_served_verb_leaves_a_start_line_and_a_result_line() {
        let mut req = Request::new("mail_send", Value::Null);
        req.caller = Some("quaqua-rs-lead".into());
        let mut log = Vec::new();
        let ok = traced(&req, &mut log, || Ok(Value::Null));
        let bad = traced(&req, &mut log, || Err(internal("boom".into())));
        assert!(ok.is_ok() && bad.is_err());
        let text = String::from_utf8(log).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 4, "{text}");
        assert_eq!(lines[0], "quaqua daemon: verb start caller=\"quaqua-rs-lead\" verb=\"mail_send\"");
        assert!(lines[1].starts_with("quaqua daemon: verb caller=\"quaqua-rs-lead\" verb=\"mail_send\" ms="));
        assert!(lines[1].ends_with(" ok"), "{}", lines[1]);
        assert!(lines[3].ends_with(" err"), "{}", lines[3]);
    }

    fn traced_log(verb: &str, caller: &str, res: Result<Value, RpcError>) -> String {
        let mut req = Request::new(verb, Value::Null);
        req.caller = Some(caller.into());
        let mut log = Vec::new();
        let _ = traced(&req, &mut log, || res);
        String::from_utf8(log).unwrap()
    }

    #[test]
    fn a_polling_verb_that_succeeds_quickly_logs_nothing() {
        for v in ["status", "ping", caller::VERB, "mail.inbox"] {
            let text = traced_log(v, "lead", Ok(Value::Null));
            assert_eq!(text, "", "{v}");
        }
    }

    #[test]
    fn a_slow_polling_verb_logs_one_result_line() {
        let mut req = Request::new("status", Value::Null);
        req.caller = Some("lead".into());
        let mut log = Vec::new();
        let _ = traced_with(&req, &mut log, 0, || Ok(Value::Null));
        let text = String::from_utf8(log).unwrap();
        assert_eq!(text.lines().count(), 1, "{text}");
        assert!(text.contains("verb=\"status\"") && text.trim_end().ends_with(" ok"), "{text}");
        let mut log = Vec::new();
        let _ = traced_with(&req, &mut log, u128::MAX, || Ok(Value::Null));
        assert!(log.is_empty());
    }

    #[test]
    fn a_polling_verb_that_fails_logs_its_result() {
        let text = traced_log("status", "lead", Err(internal("boom".into())));
        assert!(text.contains("verb=\"status\"") && text.trim_end().ends_with(" err"), "{text}");
    }

    #[test]
    fn a_mutating_verb_that_succeeds_logs_both_lines() {
        for v in ["mail.send", "mail.ack"] {
            let text = traced_log(v, "lead", Ok(Value::Null));
            assert_eq!(text.lines().count(), 2, "{v}: {text}");
        }
    }

    #[test]
    fn a_newline_in_the_verb_cannot_forge_a_line() {
        let text = traced_log("x\nquaqua daemon: verb caller=root verb=stop ms=0 ok", "lead", Ok(Value::Null));
        assert_eq!(text.lines().count(), 2, "{text:?}");
        assert!(text.lines().all(|l| l.starts_with("quaqua daemon: verb")), "{text:?}");
        assert!(!text.contains("x\nquaqua"), "{text:?}");
    }

    #[test]
    fn a_newline_in_the_caller_cannot_forge_a_line() {
        let text = traced_log("mail.send", "x\nquaqua daemon: verb caller=root verb=stop ms=0 ok", Ok(Value::Null));
        assert_eq!(text.lines().count(), 2, "{text:?}");
        assert!(text.lines().all(|l| l.starts_with("quaqua daemon: verb")), "{text:?}");
        assert!(!text.contains("x\nquaqua"), "{text:?}");
    }

    struct Fixed(Result<&'static str, ()>);
    impl PeerResolver for Fixed {
        fn resolve(&self, _: peer::PeerCred) -> Result<CallerId, RpcError> {
            self.0
                .map(|n| CallerId(n.into()))
                .map_err(|()| RpcError::new(peer::UNMAPPED_PEER, "no pane"))
        }
    }

    fn run(verb: &str, who: Result<&'static str, ()>, uid: u32) -> Result<Request, RpcError> {
        let mut req = Request::new(verb, Value::Null);
        req.caller = Some("spoof".into());
        let cred = Ok(PeerCred { pid: 1, uid });
        identify(req, Some(&Fixed(who)), Some(&cred), 1000)
    }

    #[test]
    fn other_uid_is_refused_for_every_verb_even_when_mapped() {
        for v in ["ping", "stop", "mail.send"] {
            let e = run(v, Ok("w-1"), 0).unwrap_err();
            assert_eq!(e.code, peer::FOREIGN_UID, "{v}");
        }
    }

    #[test]
    fn a_peer_the_resolver_cannot_name_is_refused_for_every_verb() {
        for v in ["ping", "stop", "status", "mail.send"] {
            let e = run(v, Err(()), 1000).unwrap_err();
            assert_eq!(e.code, peer::UNMAPPED_PEER, "{v}");
        }
    }

    /// Issue #368: the Principal runs the farm from a plain terminal, but names
    /// no agent, so every agent-only verb stays closed to it.
    #[test]
    fn the_principal_outside_a_pane_is_refused_every_agent_verb() {
        for v in [
            "task.start",
            "task.create",
            "worker.stop",
            "mail.inbox",
            "mail.send",
            "push",
            "push.cos",
            "merge",
            "pr.merge",
            "memory.write",
            "pipeline",
            "signal",
            "watch.agent",
            "watch.report",
            "workflow.start",
        ] {
            let e = run(v, Ok(PRINCIPAL), 1000).unwrap_err();
            assert_eq!(e.code, AGENT_ONLY, "{v}");
        }
    }

    /// The verbs the Principal needs from its own terminal: the launcher asks
    /// `ping` before it starts a daemon, and the owner stops and configures the
    /// farm from there. None of them names an agent, so the gate names it the
    /// Principal and no agent verb comes with it.
    #[test]
    fn the_principal_outside_a_pane_may_ping_status_stop_and_write_config() {
        for v in ["ping", "status", "stop", "config", "roster", caller::VERB] {
            let req = run(v, Ok(PRINCIPAL), 1000).unwrap_or_else(|e| panic!("{v}: {e:?}"));
            assert!(is_principal(&req), "{v} is not marked the principal");
            assert_eq!(req.caller.as_deref(), Some(PRINCIPAL), "{v}");
            assert_eq!(req.caller_pane, None, "{v}");
            // Its name is not an agent's: the gate still names nobody.
            assert_eq!(caller::identified(&req), None, "{v}");
        }
    }

    /// Review finding 4: the name alone is not the Principal. Only the gate sets
    /// it, so a daemon with no resolver (a claim the daemon trusts) cannot reach
    /// the Principal's config-write shortcut through `QQ_AGENT=owner`.
    #[test]
    fn a_claim_of_the_principal_name_is_not_the_principal() {
        let mut req = Request::new("config", Value::Null);
        req.caller = Some(PRINCIPAL.into());
        assert!(!is_principal(&req), "a claim is not the gate's verdict");
    }

    /// A pane-hosted agent that happens to be called `owner` keeps agent
    /// rights only: the Principal is never a name a pane can hand out.
    #[test]
    fn an_agent_named_owner_is_no_principal() {
        struct InPane;
        impl PeerResolver for InPane {
            fn resolve(&self, _: peer::PeerCred) -> Result<CallerId, RpcError> {
                Ok(CallerId(PRINCIPAL.into()))
            }
            fn resolve_pane(
                &self,
                _: peer::PeerCred,
            ) -> Result<(CallerId, Option<String>), RpcError> {
                Ok((CallerId(PRINCIPAL.into()), Some("p1".into())))
            }
        }
        let mut req = Request::new("task.start", Value::Null);
        req.caller = Some("spoof".into());
        let cred = Ok(PeerCred { pid: 1, uid: 1000 });
        let req = identify(req, Some(&InPane), Some(&cred), 1000).unwrap();
        assert!(!is_principal(&req));
        assert_eq!(caller::identified(&req), Some(PRINCIPAL));
    }
}
