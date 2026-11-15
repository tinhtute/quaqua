//! Production clocks for the workflow engine.
//!
//! [`SystemClock`] answers timestamp queries. [`RealClock`] is the production
//! `RunnerClock`: it holds timers and reports their deadlines but never sleeps.
//! The caller owns waiting: sleep until [`RealClock::next_deadline`], then let
//! the runner fire what is due.

use std::cmp::Reverse;
use std::collections::BinaryHeap;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::engine::runner::RunnerClock;

/// Real-time clock backed by [`SystemTime`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SystemClock;

impl SystemClock {
    /// Creates a new system clock instance.
    pub fn new() -> Self {
        Self
    }

    /// Returns the current system time in milliseconds since the Unix epoch.
    pub fn now_ms(&self) -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0)
    }

    /// Returns the current system time as [`SystemTime`].
    pub fn now(&self) -> SystemTime {
        SystemTime::now()
    }
}

/// Real-time clock with scheduled timers implementing `RunnerClock`.
///
/// Timers sit in a min-heap ordered by `(deadline_ms, insert_seq)`. Deadlines are
/// Unix epoch milliseconds, the same unit as [`RealClock::now_ms`] and journal
/// timestamps. The clock never sleeps; see the module docs.
#[derive(Debug, Clone, Default)]
pub struct RealClock {
    timers: BinaryHeap<Reverse<(u64, u64, String)>>,
    seq: u64,
}

impl RealClock {
    /// Creates a new real-time clock instance with no scheduled timers.
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns the current system time in milliseconds since Unix epoch.
    pub fn now_ms(&self) -> u64 {
        SystemClock.now_ms()
    }

    /// Schedules a timer with an ID that fires `duration` from now.
    pub fn schedule_timer(&mut self, id: &str, duration: Duration) {
        let deadline = self.now_ms().saturating_add(duration.as_millis() as u64);
        self.schedule_timer_at(id, deadline);
    }

    /// Schedules a timer with an explicit epoch-ms deadline.
    pub fn schedule_timer_at(&mut self, id: &str, deadline_ms: u64) {
        self.seq = self.seq.wrapping_add(1);
        self.timers
            .push(Reverse((deadline_ms, self.seq, id.to_string())));
    }

    /// Epoch-ms deadline of the earliest scheduled timer, if any.
    pub fn next_deadline(&self) -> Option<u64> {
        self.timers.peek().map(|Reverse((d, _, _))| *d)
    }

    /// Drains and returns the IDs of every timer whose deadline is at or before
    /// `now_ms`, earliest first. Returns an empty vec when none is due.
    pub fn fire_due(&mut self, now_ms: u64) -> Vec<String> {
        let mut fired = Vec::new();
        while let Some(Reverse((deadline, _, _))) = self.timers.peek() {
            if *deadline > now_ms {
                break;
            }
            if let Some(Reverse((_, _, id))) = self.timers.pop() {
                fired.push(id);
            }
        }
        fired
    }

    /// Returns the number of scheduled timers waiting to fire.
    pub fn pending_timers_count(&self) -> usize {
        self.timers.len()
    }
}

impl RunnerClock for RealClock {
    fn now_ms(&self) -> u64 {
        self.now_ms()
    }

    fn schedule_timer(&mut self, id: &str, duration: Duration) {
        self.schedule_timer(id, duration);
    }

    fn next_deadline(&self) -> Option<u64> {
        self.next_deadline()
    }

    fn fire_due(&mut self, now_ms: u64) -> Vec<String> {
        self.fire_due(now_ms)
    }

    fn pending_timers_count(&self) -> usize {
        self.pending_timers_count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::mocks::VirtualClock;
    use std::thread::sleep;
    use std::time::{Duration, Instant};

    #[test]
    fn test_system_clock_now_ms_positive_and_advancing() {
        let clock = SystemClock::new();
        let t1 = clock.now_ms();
        assert!(t1 > 0, "system clock now_ms must be positive");

        sleep(Duration::from_millis(5));
        let t2 = clock.now_ms();
        assert!(t2 >= t1, "system clock now_ms must advance or stay equal");
    }

    #[test]
    fn test_system_clock_now_system_time() {
        let clock = SystemClock::new();
        let now = clock.now();
        let elapsed = SystemTime::now()
            .duration_since(now)
            .expect("now() must be in past or current");
        assert!(elapsed < Duration::from_secs(5));
    }

    #[test]
    fn test_system_clock_default() {
        let clock = SystemClock;
        assert!(clock.now_ms() > 0);
    }

    #[test]
    fn test_real_clock_fire_due_returns_only_due_timers_in_deadline_order() {
        let mut clock = RealClock::new();
        clock.schedule_timer_at("late", 3_000);
        clock.schedule_timer_at("early", 1_000);
        clock.schedule_timer_at("mid", 2_000);

        assert_eq!(clock.next_deadline(), Some(1_000));
        assert!(clock.fire_due(999).is_empty());
        assert_eq!(clock.fire_due(2_000), vec!["early", "mid"]);
        assert_eq!(clock.pending_timers_count(), 1);
        assert_eq!(clock.next_deadline(), Some(3_000));
        assert_eq!(clock.fire_due(u64::MAX), vec!["late"]);
        assert_eq!(clock.next_deadline(), None);
    }

    #[test]
    fn test_real_clock_equal_deadlines_fire_in_insert_order() {
        let mut clock = RealClock::new();
        clock.schedule_timer_at("a", 500);
        clock.schedule_timer_at("b", 500);
        assert_eq!(clock.fire_due(500), vec!["a", "b"]);
    }

    #[test]
    fn test_real_clock_far_timer_never_blocks_the_caller() {
        let mut clock = RealClock::new();
        clock.schedule_timer("hour", Duration::from_secs(3600));
        let started = Instant::now();
        let fired = clock.fire_due(clock.now_ms());
        assert!(fired.is_empty());
        assert!(started.elapsed() < Duration::from_millis(50));
        let deadline = clock.next_deadline().expect("timer pending");
        assert!(deadline >= clock.now_ms() + 3_599_000);
    }

    #[test]
    fn test_virtual_clock_fire_due_advances_and_never_goes_back() {
        let mut clock = VirtualClock::new(10_000);
        clock.schedule_timer("t", Duration::from_secs(5));
        assert_eq!(clock.next_deadline(), Some(15_000));

        assert!(clock.fire_due(12_000).is_empty());
        assert_eq!(clock.now_ms(), 12_000);
        assert!(clock.fire_due(11_000).is_empty());
        assert_eq!(clock.now_ms(), 12_000, "virtual time never runs backwards");
        assert_eq!(clock.fire_due(15_000), vec!["t"]);
        assert_eq!(clock.next_deadline(), None);
    }

    #[test]
    fn test_real_clock_implements_runner_clock() {
        fn check_send_and_runner_clock<T: RunnerClock + Send>(_val: &T) {}
        check_send_and_runner_clock(&RealClock::new());
    }
}
