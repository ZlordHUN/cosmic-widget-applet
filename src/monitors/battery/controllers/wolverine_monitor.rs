// SPDX-License-Identifier: MPL-2.0

//! Independent controller polling, woken by matching USB/HID hotplug events.

use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use crate::monitors::battery::{
    BatteryDevice, NATIVE_POLL_INTERVAL, events, merge_native_wolverine, query_native_wolverine,
};

const EVENT_QUERY_INTERVAL: Duration = Duration::from_secs(1);
const ENUMERATION_SETTLE_DELAY: Duration = Duration::from_millis(350);

// The outer option distinguishes an uninitialized reader from a confirmed
// absence. Errors leave the last successful observation intact.
type Snapshot = Option<Option<BatteryDevice>>;

#[derive(Clone)]
pub(in crate::monitors::battery) struct Monitor {
    snapshot: Arc<Mutex<Snapshot>>,
}

impl Monitor {
    pub(in crate::monitors::battery) fn new() -> Self {
        let snapshot: Arc<Mutex<Snapshot>> = Arc::new(Mutex::new(None));
        let weak_snapshot = Arc::downgrade(&snapshot);
        thread::spawn(move || {
            let origin = Instant::now();
            let mut scheduler = Scheduler::new();
            let mut listener = events::Listener::new().ok();
            let mut announced = false;
            let mut warned_query_failure = false;

            loop {
                if weak_snapshot.strong_count() == 0 {
                    break;
                }
                let timeout = scheduler.next_deadline().saturating_sub(origin.elapsed());
                let changes = if let Some(listener) = listener.as_mut() {
                    listener.wait(timeout)
                } else {
                    thread::sleep(timeout);
                    events::Changes::default()
                };
                let now = origin.elapsed();
                if changes.wolverine {
                    scheduler.on_event(now);
                }
                let Some(query) = scheduler.take_due(now) else {
                    continue;
                };
                let Some(snapshot) = weak_snapshot.upgrade() else {
                    break;
                };
                let was_connected = snapshot
                    .lock()
                    .unwrap()
                    .as_ref()
                    .and_then(Option::as_ref)
                    .is_some_and(|device| device.is_connected);
                // Do not keep the monitor alive, or hold its lock, during HID I/O.
                drop(snapshot);
                match query_native_wolverine(was_connected) {
                    Ok(state) => {
                        let Some(snapshot) = weak_snapshot.upgrade() else {
                            break;
                        };
                        if !announced && state.as_ref().is_some_and(|device| device.is_connected) {
                            log::info!("Using native monitoring for Razer Wolverine V3 Pro 8K PC");
                            announced = true;
                        }
                        let mut snapshot = snapshot.lock().unwrap();
                        if snapshot.as_ref() != Some(&state) {
                            log::debug!("Native Wolverine battery update: {state:?}");
                            *snapshot = Some(state);
                        }
                    }
                    Err(error) if !warned_query_failure => {
                        log::warn!("Native Wolverine battery query failed: {error}");
                        warned_query_failure = true;
                    }
                    Err(_) => {}
                }
                scheduler.finished(query, origin.elapsed());
            }
        });
        Self { snapshot }
    }

    pub(in crate::monitors::battery) fn apply(&self, devices: &mut Vec<BatteryDevice>) {
        let snapshot = self.snapshot.lock().unwrap().clone();
        if let Some(state) = snapshot {
            merge_native_wolverine(devices, state);
        }
    }

    #[cfg(test)]
    pub(in crate::monitors::battery) fn with_snapshot(snapshot: Snapshot) -> Self {
        Self {
            snapshot: Arc::new(Mutex::new(snapshot)),
        }
    }

    #[cfg(test)]
    pub(in crate::monitors::battery) fn replace_snapshot(&self, snapshot: Snapshot) {
        *self.snapshot.lock().unwrap() = snapshot;
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Query {
    Regular,
    Event,
    Settle,
}

/// Deadlines are relative to the worker's start. An unrelated event cannot move
/// a normal query later, and hotplug bursts share a single settling retry.
struct Scheduler {
    regular_at: Duration,
    event_at: Option<Duration>,
    settle_at: Option<Duration>,
    next_event_batch: Duration,
}

impl Scheduler {
    fn new() -> Self {
        Self {
            regular_at: Duration::ZERO,
            event_at: None,
            settle_at: None,
            next_event_batch: Duration::ZERO,
        }
    }

    fn next_deadline(&self) -> Duration {
        self.event_at
            .into_iter()
            .chain(self.settle_at)
            .fold(self.regular_at, Duration::min)
    }

    fn on_event(&mut self, now: Duration) {
        // The pending query/retry will inspect the final topology after a batch
        // of interface events. Do not extend it every time another event arrives.
        if self.event_at.is_some() || self.settle_at.is_some() {
            return;
        }
        self.event_at = Some(now.max(self.next_event_batch));
    }

    fn take_due(&mut self, now: Duration) -> Option<Query> {
        let regular_due = now >= self.regular_at;
        if regular_due {
            // Advance the absolute cadence, including when a HID operation took
            // longer than one interval. Avoid accumulating missed requests.
            while self.regular_at <= now {
                self.regular_at += NATIVE_POLL_INTERVAL;
            }
        }
        if self.event_at.is_some_and(|deadline| now >= deadline) {
            self.event_at = None;
            self.settle_at = None;
            self.next_event_batch = now + EVENT_QUERY_INTERVAL;
            return Some(Query::Event);
        }
        if self.settle_at.is_some_and(|deadline| now >= deadline) {
            self.settle_at = None;
            return Some(Query::Settle);
        }
        regular_due.then_some(Query::Regular)
    }

    fn finished(&mut self, query: Query, now: Duration) {
        // A slow HID read may span one or more regular deadlines. Skip those
        // missed slots while preserving the original phase, so failures cannot
        // turn the five-second fallback into continuous catch-up polling.
        while self.regular_at <= now {
            self.regular_at += NATIVE_POLL_INTERVAL;
        }
        if query == Query::Event {
            // USB permissions and the controller's firmware can settle after
            // enumeration. Retry once even when the first query returned an old
            // but otherwise valid wireless/disconnected reading.
            self.settle_at = Some(now + ENUMERATION_SETTLE_DELAY);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{ENUMERATION_SETTLE_DELAY, NATIVE_POLL_INTERVAL, Query, Scheduler};
    use std::time::Duration;

    fn milliseconds(value: u64) -> Duration {
        Duration::from_millis(value)
    }

    fn started_scheduler() -> Scheduler {
        let mut scheduler = Scheduler::new();
        assert_eq!(scheduler.take_due(Duration::ZERO), Some(Query::Regular));
        scheduler.finished(Query::Regular, milliseconds(80));
        scheduler
    }

    #[test]
    fn controller_event_queries_before_regular_deadline_and_preserves_cadence() {
        let mut scheduler = started_scheduler();
        scheduler.on_event(milliseconds(4_800));
        assert_eq!(scheduler.next_deadline(), milliseconds(4_800));
        assert_eq!(scheduler.take_due(milliseconds(4_800)), Some(Query::Event));
        scheduler.finished(Query::Event, milliseconds(4_880));

        assert_eq!(scheduler.next_deadline(), NATIVE_POLL_INTERVAL);
        assert_eq!(
            scheduler.take_due(milliseconds(5_000)),
            Some(Query::Regular)
        );
        scheduler.finished(Query::Regular, milliseconds(5_080));
        assert_eq!(scheduler.next_deadline(), milliseconds(5_230));
    }

    #[test]
    fn unrelated_wakeups_do_not_query_or_postpone_regular_deadline() {
        let mut scheduler = started_scheduler();
        for now in [100, 500, 1_000, 4_999] {
            assert_eq!(scheduler.take_due(milliseconds(now)), None);
            assert_eq!(scheduler.next_deadline(), NATIVE_POLL_INTERVAL);
        }
        assert_eq!(
            scheduler.take_due(milliseconds(5_000)),
            Some(Query::Regular)
        );
        assert_eq!(scheduler.next_deadline(), milliseconds(10_000));
    }

    #[test]
    fn controller_event_bursts_coalesce_and_rate_limit_query_batches() {
        let mut scheduler = started_scheduler();
        scheduler.on_event(milliseconds(100));
        assert_eq!(scheduler.take_due(milliseconds(100)), Some(Query::Event));
        scheduler.finished(Query::Event, milliseconds(180));
        for now in [200, 300, 400, 500] {
            scheduler.on_event(milliseconds(now));
            assert_eq!(scheduler.take_due(milliseconds(now)), None);
        }
        assert_eq!(scheduler.next_deadline(), milliseconds(530));
        assert_eq!(scheduler.take_due(milliseconds(530)), Some(Query::Settle));
        scheduler.finished(Query::Settle, milliseconds(610));

        for now in [620, 700, 800, 1_099] {
            scheduler.on_event(milliseconds(now));
            assert_eq!(scheduler.take_due(milliseconds(now)), None);
            assert_eq!(scheduler.next_deadline(), milliseconds(1_100));
        }
        assert_eq!(scheduler.take_due(milliseconds(1_100)), Some(Query::Event));
    }

    #[test]
    fn missing_events_still_query_on_five_second_deadlines() {
        let mut scheduler = started_scheduler();
        assert_eq!(scheduler.take_due(milliseconds(4_999)), None);
        assert_eq!(
            scheduler.take_due(milliseconds(5_000)),
            Some(Query::Regular)
        );
        scheduler.finished(Query::Regular, milliseconds(5_080));
        assert_eq!(scheduler.next_deadline(), milliseconds(10_000));
        // A slow query skips missed intervals instead of producing a busy loop.
        assert_eq!(
            scheduler.take_due(milliseconds(21_000)),
            Some(Query::Regular)
        );
        assert_eq!(scheduler.next_deadline(), milliseconds(25_000));
        assert_eq!(scheduler.take_due(milliseconds(21_000)), None);
    }

    #[test]
    fn slow_query_completion_skips_missed_deadlines_without_catch_up_polling() {
        let mut scheduler = Scheduler::new();
        assert_eq!(scheduler.take_due(Duration::ZERO), Some(Query::Regular));
        scheduler.finished(Query::Regular, milliseconds(6_000));
        assert_eq!(scheduler.next_deadline(), milliseconds(10_000));
        assert_eq!(scheduler.take_due(milliseconds(6_000)), None);
        assert_eq!(
            scheduler.take_due(milliseconds(10_000)),
            Some(Query::Regular)
        );
        scheduler.finished(Query::Regular, milliseconds(21_000));
        assert_eq!(scheduler.next_deadline(), milliseconds(25_000));
        assert_eq!(scheduler.take_due(milliseconds(21_000)), None);
    }

    #[test]
    fn enumeration_retry_is_delayed_and_bounded_to_one_per_event_batch() {
        let mut scheduler = started_scheduler();
        scheduler.on_event(milliseconds(100));
        assert_eq!(scheduler.take_due(milliseconds(100)), Some(Query::Event));
        scheduler.finished(Query::Event, milliseconds(180));
        let retry_at = milliseconds(180) + ENUMERATION_SETTLE_DELAY;
        assert_eq!(scheduler.next_deadline(), retry_at);
        assert_eq!(scheduler.take_due(retry_at - milliseconds(1)), None);
        assert_eq!(scheduler.take_due(retry_at), Some(Query::Settle));
        scheduler.finished(Query::Settle, retry_at + milliseconds(80));
        assert_eq!(scheduler.next_deadline(), NATIVE_POLL_INTERVAL);
        assert_eq!(scheduler.take_due(milliseconds(1_000)), None);
    }

    #[test]
    fn event_and_regular_deadline_share_one_hardware_query() {
        let mut scheduler = started_scheduler();
        scheduler.on_event(NATIVE_POLL_INTERVAL);
        assert_eq!(scheduler.take_due(NATIVE_POLL_INTERVAL), Some(Query::Event));
        assert_eq!(scheduler.take_due(NATIVE_POLL_INTERVAL), None);
        scheduler.finished(Query::Event, milliseconds(5_080));
        assert_eq!(scheduler.next_deadline(), milliseconds(5_430));
    }
}
