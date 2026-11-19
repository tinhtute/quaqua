//! One loop per workflow: `recv(event | next deadline) -> resume`. A loop
//! blocks on its channel until a pushed event arrives or its next timer is
//! due; it never wakes on a tick, so an idle workflow adds no journal rows.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use serde_json::Value;

use super::store::{SharedJournal, Store};
use crate::engine::clock::RealClock;
use crate::engine::journal::Journal;
use crate::engine::runner::{run, run_until_blocked, RunnerOptions, RunnerOutcome};
use crate::engine::{Activity, Workflow, WorkflowEvent};

/// Where a workflow stands after one advance.
#[derive(Debug, Clone, PartialEq)]
pub enum Progress {
    Running,
    Completed,
    Failed(String),
}

/// Type-erased workflow runner the loop drives.
pub trait WorkflowDriver: Send {
    /// Delivers `events` (possibly none: a due timer) and runs to the next block.
    fn advance(
        &mut self,
        journal: &mut dyn Journal,
        events: Vec<WorkflowEvent>,
    ) -> Result<Progress, String>;
    /// Epoch-ms deadline of the earliest pending timer after the last advance.
    fn next_deadline(&self) -> Option<u64>;
}

/// [`WorkflowDriver`] over a typed [`Workflow`] and a real clock.
pub struct Machine<W: Workflow> {
    workflow: W,
    id: String,
    initial: Option<W::State>,
    activities: Box<dyn Activity>,
    deadline: Option<u64>,
}

impl<W: Workflow> Machine<W> {
    /// A new workflow: `initial` is the replay base written on first advance.
    pub fn new(workflow: W, id: &str, initial: W::State, activities: Box<dyn Activity>) -> Self {
        Machine {
            workflow,
            id: id.to_string(),
            initial: Some(initial),
            activities,
            deadline: None,
        }
    }

    /// A workflow whose base already sits in the journal (daemon restart).
    pub fn resume_only(workflow: W, id: &str, activities: Box<dyn Activity>) -> Self {
        Machine {
            workflow,
            id: id.to_string(),
            initial: None,
            activities,
            deadline: None,
        }
    }
}

impl<W: Workflow> WorkflowDriver for Machine<W> {
    fn advance(
        &mut self,
        journal: &mut dyn Journal,
        events: Vec<WorkflowEvent>,
    ) -> Result<Progress, String> {
        // A fresh clock per advance: replay re-arms the pending timers on it.
        let mut clock = RealClock::new();
        let has_base = journal
            .load_latest_snapshot(&self.id)
            .map_err(|e| e.to_string())?
            .is_some();
        // `run` writes the replay base and the first blocked snapshot. Later
        // advances skip the blocked snapshot: a wake with no new event would
        // append the same seq again and hit the snapshots primary key.
        let opts = RunnerOptions {
            snapshot_at_blocked: false,
            ..RunnerOptions::default()
        };
        let mut events = Some(events);
        let resume = |wf: &W,
                      journal: &mut dyn Journal,
                      acts: &mut dyn Activity,
                      clock: &mut RealClock,
                      ev| {
            run_until_blocked(wf, &self.id, None, journal, acts, clock, &opts, ev)
        };
        let outcome = if has_base {
            resume(
                &self.workflow,
                journal,
                &mut *self.activities,
                &mut clock,
                events.take().unwrap_or_default(),
            )
        } else {
            let initial = self
                .initial
                .clone()
                .ok_or_else(|| format!("{}: no journal base and no initial state", self.id))?;
            let first = run(
                &self.workflow,
                &self.id,
                initial,
                journal,
                &mut *self.activities,
                &mut clock,
            );
            match (first, events.take().filter(|e| !e.is_empty())) {
                (Ok(RunnerOutcome::Blocked(_)), Some(ev)) => resume(
                    &self.workflow,
                    journal,
                    &mut *self.activities,
                    &mut clock,
                    ev,
                ),
                (first, _) => first,
            }
        }
        .map_err(|e| e.to_string())?;
        self.deadline = clock.next_deadline();
        Ok(match outcome {
            RunnerOutcome::Completed(_) => Progress::Completed,
            RunnerOutcome::Failed { code, reason } => Progress::Failed(format!("{code}: {reason}")),
            RunnerOutcome::Blocked(_) => Progress::Running,
        })
    }

    fn next_deadline(&self) -> Option<u64> {
        self.deadline
    }
}

/// Builds a driver for `(workflow id, input)`; `None` input means a restart
/// that must resume from the journal.
pub type Factory =
    Arc<dyn Fn(&str, Option<&Value>) -> Result<Box<dyn WorkflowDriver>, String> + Send + Sync>;

enum Msg {
    Event(Box<WorkflowEvent>),
    Stop,
}

struct Slot {
    tx: Sender<Msg>,
    handle: JoinHandle<()>,
}

/// Called once for each workflow whose loop is newly live: created by `start`
/// or revived by `recover`. Runs with no supervisor lock held.
pub type LiveHook = Arc<dyn Fn(&Supervisor, &Store, &str) + Send + Sync>;

pub struct Supervisor {
    store: Arc<Store>,
    live_hooks: Mutex<Vec<LiveHook>>,
    kinds: HashMap<String, Factory>,
    loops: Mutex<HashMap<String, Slot>>,
    /// Set by `shutdown` under the `loops` lock: no loop starts after it.
    closed: AtomicBool,
}

fn now_ms() -> u64 {
    crate::engine::clock::SystemClock.now_ms()
}

impl Supervisor {
    pub fn new(store: Arc<Store>, kinds: HashMap<String, Factory>) -> Arc<Self> {
        Arc::new(Supervisor {
            store,
            live_hooks: Mutex::new(Vec::new()),
            kinds,
            loops: Mutex::new(HashMap::new()),
            closed: AtomicBool::new(false),
        })
    }

    fn loops(&self) -> std::sync::MutexGuard<'_, HashMap<String, Slot>> {
        self.loops.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Adds a hook run for every workflow that goes live from now on.
    pub fn on_live(&self, hook: LiveHook) {
        self.live_hooks
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .push(hook);
    }

    fn fire_live(&self, id: &str) {
        let hooks = self
            .live_hooks
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone();
        for h in hooks {
            h(self, &self.store, id);
        }
    }

    /// Starts a workflow loop. A repeat of a live or finished id changes
    /// nothing and returns `Ok(false)`.
    pub fn start(&self, kind: &str, id: &str, input: &Value) -> Result<bool, String> {
        let factory = self
            .kinds
            .get(kind)
            .ok_or_else(|| format!("unknown workflow kind '{kind}'"))?;
        // One lock from the check to the insert: two starts of one id cannot both pass.
        let mut loops = self.loops();
        if self.closed.load(Ordering::SeqCst) {
            return Err("daemon is shutting down".to_string());
        }
        if loops.contains_key(id) {
            return Ok(false);
        }
        if let Some(row) = self.store.workflow(id)? {
            if row.finished_ms.is_some() {
                return Ok(false);
            }
        }
        let mut driver = factory(id, Some(input))?;
        // The first advance writes the replay base. It comes before the row,
        // so `recover` never meets a row that has no base to resume from.
        let progress = driver.advance(&mut self.store.journal(), Vec::new())?;
        self.store.register_workflow(id, kind, now_ms())?;
        let live = progress == Progress::Running;
        match progress {
            Progress::Running => self.spawn_loop(&mut loops, id, driver),
            Progress::Completed => self.store.finish_workflow(id, "completed", now_ms())?,
            Progress::Failed(reason) => {
                eprintln!("quaqua daemon: workflow {id} failed: {reason}");
                self.store.finish_workflow(id, "failed", now_ms())?;
            }
        }
        drop(loops);
        if live {
            self.fire_live(id);
        }
        Ok(true)
    }

    /// Restarts the loop of every unfinished workflow whose kind is registered.
    pub fn recover(&self) -> Result<Vec<String>, String> {
        let mut revived = Vec::new();
        for row in self.store.workflows()? {
            let mut loops = self.loops();
            if row.finished_ms.is_some() || loops.contains_key(&row.id) {
                continue;
            }
            let Some(factory) = self.kinds.get(&row.kind) else {
                eprintln!("quaqua daemon: no kind '{}' for workflow {}", row.kind, row.id);
                continue;
            };
            match factory(&row.id, None) {
                Ok(driver) => {
                    self.spawn_loop(&mut loops, &row.id, driver);
                    drop(loops);
                    self.fire_live(&row.id);
                    revived.push(row.id);
                }
                Err(e) => eprintln!("quaqua daemon: cannot recover {}: {e}", row.id),
            }
        }
        Ok(revived)
    }

    /// Pushes an observation into a running workflow's loop.
    pub fn signal(&self, id: &str, source: &str, name: &str, payload: Value) -> Result<(), String> {
        let loops = self.loops();
        let slot = loops
            .get(id)
            .ok_or_else(|| format!("workflow '{id}' is not running"))?;
        slot.tx
            .send(Msg::Event(Box::new(WorkflowEvent::Signal {
                source: source.to_string(),
                name: name.to_string(),
                payload,
            })))
            .map_err(|_| format!("workflow '{id}' is not running"))
    }

    pub fn is_running(&self, id: &str) -> bool {
        self.loops()
            .get(id)
            .is_some_and(|s| !s.handle.is_finished())
    }

    /// Stops every loop and waits for it. Workflows stay unfinished in the
    /// database, so the next start resumes them.
    pub fn shutdown(&self) {
        let slots: Vec<Slot> = {
            let mut loops = self.loops();
            self.closed.store(true, Ordering::SeqCst);
            loops.drain().map(|(_, s)| s).collect()
        };
        for s in &slots {
            let _ = s.tx.send(Msg::Stop);
        }
        for s in slots {
            let _ = s.handle.join();
        }
    }

    fn spawn_loop(
        &self,
        loops: &mut HashMap<String, Slot>,
        id: &str,
        driver: Box<dyn WorkflowDriver>,
    ) {
        let (tx, rx) = channel::<Msg>();
        let store = Arc::clone(&self.store);
        let journal = store.journal();
        let wf = id.to_string();
        let handle = std::thread::spawn(move || run_loop(&wf, driver, rx, &store, journal));
        loops.insert(id.to_string(), Slot { tx, handle });
    }
}

fn run_loop(
    id: &str,
    mut driver: Box<dyn WorkflowDriver>,
    rx: std::sync::mpsc::Receiver<Msg>,
    store: &Store,
    mut journal: SharedJournal,
) {
    let mut events: Vec<WorkflowEvent> = Vec::new();
    let mut stopping = false;
    loop {
        match driver.advance(&mut journal, std::mem::take(&mut events)) {
            Ok(Progress::Running) => {}
            Ok(Progress::Completed) => {
                let _ = store.finish_workflow(id, "completed", now_ms());
                return;
            }
            Ok(Progress::Failed(reason)) => {
                eprintln!("quaqua daemon: workflow {id} failed: {reason}");
                let _ = store.finish_workflow(id, "failed", now_ms());
                return;
            }
            // Left unfinished in the database: the next daemon start resumes it.
            Err(e) => {
                eprintln!("quaqua daemon: workflow {id} stopped: {e}");
                return;
            }
        }
        if stopping {
            return;
        }
        // Wait for a pushed event or the next deadline, whichever comes first.
        let first = match driver.next_deadline() {
            Some(d) => {
                let now = now_ms();
                if d <= now {
                    continue;
                }
                match rx.recv_timeout(Duration::from_millis(d - now)) {
                    Ok(m) => Some(m),
                    Err(RecvTimeoutError::Timeout) => continue,
                    Err(RecvTimeoutError::Disconnected) => return,
                }
            }
            None => rx.recv().ok(),
        };
        match first {
            Some(Msg::Event(e)) => events.push(*e),
            Some(Msg::Stop) | None => return,
        }
        // Fold in whatever else is already queued, in order.
        while let Ok(m) = rx.try_recv() {
            match m {
                Msg::Event(e) => events.push(*e),
                // Events already taken off the channel were acknowledged to
                // their senders: apply them once more before stopping.
                Msg::Stop => {
                    stopping = true;
                    break;
                }
            }
        }
    }
}
