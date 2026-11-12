//! Production system clock implementation for the Quaqua workflow engine.
//!
//! Provides [`SystemClock`] for timestamp queries and [`RealClock`] as the production
//! implementation of `RunnerClock` for workflow execution with scheduled timers.

use std::cmp::Reverse;
use std::collections::BinaryHeap;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

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
/// Manages scheduled timers in an in-memory priority queue ordered by monotonic deadline.
/// Timestamps for events and telemetry are Unix epoch timestamps derived from [`SystemTime`].
/// When advancing to the next timer, sleeps the calling thread using monotonic time (`Instant`)
/// for the remaining duration rather than spinning.
#[derive(Debug, Clone, Default)]
pub struct RealClock {
    // Min-heap ordered by (deadline: Instant, insert_seq: u64, timer_id: String)
    timers: BinaryHeap<Reverse<(Instant, u64, String)>>,
    seq: u64,
}

impl RealClock {
    /// Creates a new real-time clock instance with no scheduled timers.
    pub fn new() -> Self {
        Self {
            timers: BinaryHeap::new(),
            seq: 0,
        }
    }

    /// Returns the current system time in milliseconds since Unix epoch.
    pub fn now_ms(&self) -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0)
    }

    /// Schedules a timer with an ID and relative duration based on a monotonic deadline.
    pub fn schedule_timer(&mut self, id: &str, duration: Duration) {
        let deadline = Instant::now() + duration;
        self.seq = self.seq.wrapping_add(1);
        self.timers
            .push(Reverse((deadline, self.seq, id.to_string())));
    }

    /// Schedules a timer with an explicit monotonic deadline (useful for deterministic tests).
    #[cfg(test)]
    pub(crate) fn schedule_timer_at(&mut self, id: &str, deadline: Instant) {
        self.seq = self.seq.wrapping_add(1);
        self.timers
            .push(Reverse((deadline, self.seq, id.to_string())));
    }

    /// Advances real time to the earliest scheduled timer, if any, by sleeping the calling thread
    /// using monotonic time until the deadline expires, then drains and returns all expired timer IDs.
    ///
    /// Returns `None` when no timers are scheduled. Never returns `Some((_, vec![]))` while
    /// timers are pending.
    pub fn advance_to_next_timer(&mut self) -> Option<(u64, Vec<String>)> {
        if self.timers.is_empty() {
            return None;
        }

        loop {
            // Sleep until the earliest timer's monotonic deadline has passed without busy-waiting.
            while let Some(Reverse((earliest_deadline, _, _))) = self.timers.peek() {
                let now = Instant::now();
                if *earliest_deadline > now {
                    let delta = *earliest_deadline - now;
                    std::thread::sleep(delta);
                } else {
                    break;
                }
            }

            let now_instant = Instant::now();
            let mut fired = Vec::new();
            while let Some(Reverse((deadline, _, _))) = self.timers.peek() {
                if *deadline <= now_instant {
                    if let Some(Reverse((_, _, id))) = self.timers.pop() {
                        fired.push(id);
                    }
                } else {
                    break;
                }
            }

            // Guarantee non-empty return when timers are pending.
            if !fired.is_empty() {
                return Some((self.now_ms(), fired));
            }

            if self.timers.is_empty() {
                return None;
            }
        }
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

    fn advance_to_next_timer(&mut self) -> Option<(u64, Vec<String>)> {
        self.advance_to_next_timer()
    }

    fn pending_timers_count(&self) -> usize {
        self.pending_timers_count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread::sleep;
    use std::time::Duration;

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
    fn test_real_clock_timer_advance_sleeps_bounded_wall_time() {
        let mut clock = RealClock::new();
        clock.schedule_timer("t-bounded", Duration::from_millis(40));
        assert_eq!(clock.pending_timers_count(), 1);

        let start = std::time::Instant::now();
        let res = clock.advance_to_next_timer();
        let elapsed = start.elapsed();
        println!(
            "test_real_clock_timer_advance_sleeps_bounded_wall_time: elapsed wall time = {:?}",
            elapsed
        );
        let (fired_at, fired_ids) = res.unwrap();
        assert!(fired_at > 0, "fired_at must be positive epoch timestamp");
        assert_eq!(fired_ids, vec!["t-bounded"]);
        assert!(
            elapsed >= Duration::from_millis(30),
            "advance took {:?}, expected >= 30ms wall time",
            elapsed
        );
        assert!(
            elapsed < Duration::from_secs(1),
            "advance took {:?}, expected < 1s wall time",
            elapsed
        );
        assert_eq!(clock.pending_timers_count(), 0);
    }

    #[test]
    fn test_real_clock_multiple_timers_ordering() {
        let mut clock = RealClock::new();
        // Schedule in non-deadline order with wide margins: 250ms, 30ms, 120ms
        clock.schedule_timer("t-long", Duration::from_millis(250));
        clock.schedule_timer("t-short", Duration::from_millis(30));
        clock.schedule_timer("t-med", Duration::from_millis(120));
        assert_eq!(clock.pending_timers_count(), 3);

        // First timer to expire must be t-short
        let res1 = clock.advance_to_next_timer().expect("res1 should fire");
        assert_eq!(res1.1, vec!["t-short"]);
        assert_eq!(clock.pending_timers_count(), 2);

        // Second timer to expire must be t-med
        let res2 = clock.advance_to_next_timer().expect("res2 should fire");
        assert_eq!(res2.1, vec!["t-med"]);
        assert_eq!(clock.pending_timers_count(), 1);

        // Third timer to expire must be t-long
        let res3 = clock.advance_to_next_timer().expect("res3 should fire");
        assert_eq!(res3.1, vec!["t-long"]);
        assert_eq!(clock.pending_timers_count(), 0);

        // Subsequent advance on empty clock returns None
        assert_eq!(clock.advance_to_next_timer(), None);
    }

    #[test]
    fn test_real_clock_pending_count_transitions() {
        let mut clock = RealClock::new();
        assert_eq!(clock.pending_timers_count(), 0);

        // Widen margin between timers (25ms vs 120ms) to prevent preemption races
        clock.schedule_timer("timer-a", Duration::from_millis(25));
        assert_eq!(clock.pending_timers_count(), 1);

        clock.schedule_timer("timer-b", Duration::from_millis(120));
        assert_eq!(clock.pending_timers_count(), 2);

        let res_a = clock.advance_to_next_timer().expect("timer-a must fire");
        assert_eq!(res_a.1, vec!["timer-a"]);
        assert_eq!(clock.pending_timers_count(), 1);

        let res_b = clock.advance_to_next_timer().expect("timer-b must fire");
        assert_eq!(res_b.1, vec!["timer-b"]);
        assert_eq!(clock.pending_timers_count(), 0);

        assert_eq!(clock.advance_to_next_timer(), None);
        assert_eq!(clock.pending_timers_count(), 0);
    }

    #[test]
    fn test_real_clock_simultaneous_timers_drain_together() {
        let mut clock = RealClock::new();
        // Schedule against one shared monotonic deadline to prevent millisecond-tick straddling
        let target = Instant::now() + Duration::from_millis(20);
        clock.schedule_timer_at("t1", target);
        clock.schedule_timer_at("t2", target);
        assert_eq!(clock.pending_timers_count(), 2);

        let res = clock.advance_to_next_timer().expect("timers should fire");
        assert_eq!(res.1, vec!["t1", "t2"]);
        assert_eq!(clock.pending_timers_count(), 0);
    }

    #[test]
    fn test_real_clock_equal_durations_union_across_advances() {
        let mut clock = RealClock::new();
        // Independent schedule_timer calls with equal duration
        clock.schedule_timer("t1", Duration::from_millis(20));
        clock.schedule_timer("t2", Duration::from_millis(20));
        assert_eq!(clock.pending_timers_count(), 2);

        // Collect union across at most two advances
        let mut all_fired = Vec::new();
        let mut advances = 0;
        while let Some((_, fired)) = clock.advance_to_next_timer() {
            assert!(!fired.is_empty(), "fired list must never be empty");
            all_fired.extend(fired);
            advances += 1;
            if clock.pending_timers_count() == 0 || advances >= 2 {
                break;
            }
        }
        all_fired.sort();
        assert_eq!(all_fired, vec!["t1", "t2"]);
        assert_eq!(clock.pending_timers_count(), 0);
        assert!(advances <= 2);
    }

    #[test]
    fn test_real_clock_implements_runner_clock() {
        fn check_send_and_runner_clock<T: RunnerClock + Send>(_val: &T) {}
        let clock = RealClock::new();
        check_send_and_runner_clock(&clock);
    }
}
