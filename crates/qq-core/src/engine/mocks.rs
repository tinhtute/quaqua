//! In-memory engine doubles: a virtual clock, a mock activity registry and an
//! in-memory journal. They drive the real runner; there is no second runner here.

use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::engine::activity::{ActivityError, ActivityRequest, ActivityResult};
use crate::engine::journal::{EventRecord, Journal, JournalError, SnapshotRecord};

/// Discrete virtual clock without real thread sleeping.
#[derive(Debug, Clone)]
pub struct VirtualClock {
    now_ms: u64,
    // (deadline_ms, id)
    timers: BinaryHeap<Reverse<(u64, String)>>,
}

impl VirtualClock {
    /// Creates a new virtual clock initialized to `start_ms` (milliseconds since Unix epoch).
    pub fn new(start_ms: u64) -> Self {
        Self {
            now_ms: start_ms,
            timers: BinaryHeap::new(),
        }
    }

    /// Current virtual timestamp in milliseconds since Unix epoch.
    pub fn now_ms(&self) -> u64 {
        self.now_ms
    }

    /// Current virtual timestamp as [`SystemTime`].
    pub fn now(&self) -> SystemTime {
        UNIX_EPOCH + Duration::from_millis(self.now_ms)
    }

    /// Schedules a timer with an ID and relative duration.
    pub fn schedule_timer(&mut self, id: impl Into<String>, duration: Duration) {
        let deadline = self.now_ms + duration.as_millis() as u64;
        self.timers.push(Reverse((deadline, id.into())));
    }

    /// Advances virtual time by `duration` and fires all timers reaching their deadline.
    pub fn advance(&mut self, duration: Duration) -> Vec<String> {
        self.now_ms += duration.as_millis() as u64;
        self.drain_expired()
    }

    /// Advances virtual time to the earliest scheduled timer, if any, and fires expired timers.
    pub fn advance_to_next_timer(&mut self) -> Option<(u64, Vec<String>)> {
        let next_deadline = self.timers.peek().map(|Reverse((d, _))| *d)?;
        if next_deadline > self.now_ms {
            self.now_ms = next_deadline;
        }
        let fired = self.drain_expired();
        Some((self.now_ms, fired))
    }

    /// Moves virtual time forward to `ms` without firing anything; time never
    /// runs backwards. The driver uses this to idle, then lets the runner fire.
    pub fn set_now(&mut self, ms: u64) {
        self.now_ms = self.now_ms.max(ms);
    }

    /// Deadline of the earliest scheduled timer, in virtual ms.
    pub fn next_deadline(&self) -> Option<u64> {
        self.timers.peek().map(|Reverse((d, _))| *d)
    }

    /// Moves virtual time forward to `now_ms` (never back) and drains the
    /// timers that are due.
    pub fn fire_due(&mut self, now_ms: u64) -> Vec<String> {
        self.set_now(now_ms);
        self.drain_expired()
    }

    /// Number of scheduled timers waiting to fire.
    pub fn pending_timers_count(&self) -> usize {
        self.timers.len()
    }

    fn drain_expired(&mut self) -> Vec<String> {
        let mut expired = Vec::new();
        while let Some(Reverse((deadline, _))) = self.timers.peek() {
            if *deadline <= self.now_ms {
                if let Some(Reverse((_, id))) = self.timers.pop() {
                    expired.push(id);
                }
            } else {
                break;
            }
        }
        expired
    }
}

/// In-memory mock activities registry and call recorder.
#[derive(Debug, Clone, Default)]
pub struct MockActivities {
    canned_by_id: HashMap<String, ActivityResult>,
    latencies: HashMap<String, Duration>,
    default_result: Option<ActivityResult>,
    call_log: Vec<(String, ActivityRequest)>,
}

impl MockActivities {
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers a canned result for a specific activity ID.
    pub fn register(&mut self, id: impl Into<String>, result: ActivityResult) {
        self.canned_by_id.insert(id.into(), result);
    }

    /// Registers a canned result with an explicit execution latency for virtual clock progression.
    pub fn register_with_latency(
        &mut self,
        id: impl Into<String>,
        result: ActivityResult,
        latency: Duration,
    ) {
        let id_str = id.into();
        self.latencies.insert(id_str.clone(), latency);
        self.canned_by_id.insert(id_str, result);
    }

    /// Sets a default result returned when an activity ID has no explicit canned result.
    pub fn set_default(&mut self, result: ActivityResult) {
        self.default_result = Some(result);
    }

    /// Executes the activity by recording the call and returning the canned response.
    pub fn execute(
        &mut self,
        id: &str,
        req: &ActivityRequest,
    ) -> Result<ActivityResult, ActivityError> {
        self.call_log.push((id.to_string(), req.clone()));
        if let Some(res) = self.canned_by_id.get(id) {
            return Ok(res.clone());
        }
        if let Some(def) = &self.default_result {
            return Ok(def.clone());
        }
        Err(ActivityError::NotFound(format!(
            "no mock result registered for activity ID: {id}"
        )))
    }

    /// Returns the sequence of all recorded activity calls `(id, request)`.
    pub fn calls(&self) -> &[(String, ActivityRequest)] {
        &self.call_log
    }

    /// Returns the count of calls made to mock activities.
    pub fn call_count(&self) -> usize {
        self.call_log.len()
    }
}

/// In-memory implementation of [`Journal`].
#[derive(Debug, Clone, Default)]
pub struct InMemoryJournal {
    events: HashMap<String, Vec<EventRecord>>,
    snapshots: HashMap<String, Vec<SnapshotRecord>>,
}

impl InMemoryJournal {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn all_events(&self, workflow_id: &str) -> &[EventRecord] {
        self.events
            .get(workflow_id)
            .map(|v| v.as_slice())
            .unwrap_or(&[])
    }

    pub fn all_snapshots(&self, workflow_id: &str) -> &[SnapshotRecord] {
        self.snapshots
            .get(workflow_id)
            .map(|v| v.as_slice())
            .unwrap_or(&[])
    }
}

impl Journal for InMemoryJournal {
    fn append_event(
        &mut self,
        workflow_id: &str,
        event: &EventRecord,
    ) -> Result<u64, JournalError> {
        let list = self.events.entry(workflow_id.to_string()).or_default();
        let expected_seq = (list.len() + 1) as u64;
        let assigned_seq = if event.seq == 0 {
            expected_seq
        } else {
            event.seq
        };

        let mut rec = event.clone();
        rec.seq = assigned_seq;
        list.push(rec);
        Ok(assigned_seq)
    }

    fn append_snapshot(
        &mut self,
        workflow_id: &str,
        snapshot: &SnapshotRecord,
    ) -> Result<(), JournalError> {
        let list = self.snapshots.entry(workflow_id.to_string()).or_default();
        list.push(snapshot.clone());
        Ok(())
    }

    fn load_events(
        &self,
        workflow_id: &str,
        since_seq: u64,
    ) -> Result<Vec<EventRecord>, JournalError> {
        let empty = Vec::new();
        let list = self.events.get(workflow_id).unwrap_or(&empty);
        Ok(list
            .iter()
            .filter(|e| e.seq >= since_seq)
            .cloned()
            .collect())
    }

    fn load_latest_snapshot(
        &self,
        workflow_id: &str,
    ) -> Result<Option<SnapshotRecord>, JournalError> {
        Ok(self
            .snapshots
            .get(workflow_id)
            .and_then(|list| list.last().cloned()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::activity::{GitRequest, GitResult};

    #[test]
    fn test_journal_trait_is_object_safe() {
        let mut in_mem = InMemoryJournal::new();
        let journal: &mut dyn Journal = &mut in_mem;

        let event = EventRecord::new(1, 100, "test_event", r#"{"hello":"world"}"#);
        assert_eq!(journal.append_event("wf-1", &event).unwrap(), 1);
        journal
            .append_snapshot("wf-1", &SnapshotRecord::new(1, r#"{"state":"init"}"#))
            .unwrap();

        let loaded = journal.load_events("wf-1", 1).unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].kind, "test_event");
        let snap = journal.load_latest_snapshot("wf-1").unwrap().unwrap();
        assert_eq!(snap.state_json, r#"{"state":"init"}"#);
    }

    #[test]
    fn test_virtual_clock_advance_fires_timers_at_their_deadline() {
        let mut clock = VirtualClock::new(1000);
        clock.schedule_timer("timer_1", Duration::from_millis(150));

        assert!(clock.advance(Duration::from_millis(100)).is_empty());
        assert_eq!(clock.now_ms(), 1100);
        assert_eq!(clock.advance(Duration::from_millis(100)), vec!["timer_1"]);
        assert_eq!(clock.now_ms(), 1200);
    }

    #[test]
    fn test_mock_activities_return_canned_result_and_record_calls() {
        let mut mocks = MockActivities::new();
        let req = ActivityRequest::Git(GitRequest::DiffHash {
            path: "/wt".to_string(),
        });
        let canned = ActivityResult::Git(GitResult::DiffHash {
            hash: "abc".to_string(),
            stat: "1 file".to_string(),
        });
        mocks.register("diff", canned.clone());

        assert_eq!(mocks.execute("diff", &req).unwrap(), canned);
        assert!(matches!(
            mocks.execute("other", &req),
            Err(ActivityError::NotFound(_))
        ));
        assert_eq!(mocks.call_count(), 2);
        assert_eq!(mocks.calls()[0].0, "diff");
    }
}
