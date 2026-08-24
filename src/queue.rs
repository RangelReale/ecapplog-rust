//! The bounded, drop-oldest hand-off between logging threads and the worker.
//!
//! This is where the Go client's `internal/ring_buffer.go` goes. That implementation needs two
//! channels and a dedicated goroutine to get drop-oldest semantics out of Go's channels; in Rust a
//! `VecDeque` under a mutex gives it directly, because `pop_front` on a full push *is* dropping
//! the oldest.
//!
//! Two rules this module exists to enforce:
//!
//! * The caller's `on_error` callback is never invoked while the lock is held. It is arbitrary user
//!   code that may itself log, and calling it under the lock would deadlock.
//! * Every wait is bounded and wakes on shutdown, so `close()` cannot block behind a full
//!   reconnect delay. The C++ client can hang forever here: `close()` clears its running flag,
//!   then the worker thread's first statement sets it back to true, and with no server listening
//!   its quit sentinel is never dequeued.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Condvar, Mutex};
use std::time::Duration;

use crate::record::Record;

/// What a [`Queue::pop`] found.
//
// `Item` carries a whole `Record` and the other two variants carry nothing, so the enum is as large
// as the record. Boxing it -- clippy's suggestion -- would put a malloc and a free on the
// per-record path and defeat the preallocated `VecDeque`, to save moving ~200 bytes into a value
// that is matched on the very next line.
#[allow(clippy::large_enum_variant)]
pub(crate) enum Pop {
    /// An entry to send.
    Item(Record),
    /// Shutdown was requested and the queue is drained.
    Shutdown,
    /// Nothing arrived before the timeout. The worker uses these to re-check its deadlines.
    Idle,
}

struct State {
    items: VecDeque<Record>,
    /// Set while the worker holds an entry it has popped but not yet written. `flush` has to
    /// account for it, or it would report success with the last entry still in the worker's hand.
    busy: bool,
    shutdown: bool,
}

pub(crate) struct Queue {
    state: Mutex<State>,
    /// Signals "there is work, or shutdown was requested". Waited on by the worker.
    work: Condvar,
    /// Signals "the queue is empty and the worker is idle". Waited on by `flush`.
    drained: Condvar,
    capacity: usize,
    dropped: AtomicU64,
}

impl Queue {
    pub(crate) fn new(capacity: usize) -> Self {
        Self {
            state: Mutex::new(State {
                items: VecDeque::with_capacity(capacity.min(64)),
                busy: false,
                shutdown: false,
            }),
            work: Condvar::new(),
            drained: Condvar::new(),
            // A zero-capacity queue would drop everything and is far more likely a mistake than an
            // intent, so treat it as one.
            capacity: capacity.max(1),
            dropped: AtomicU64::new(0),
        }
    }

    pub(crate) fn capacity(&self) -> usize {
        self.capacity
    }

    pub(crate) fn dropped_count(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }

    /// Enqueues an entry, discarding the oldest if the queue is full.
    ///
    /// Returns the running dropped total when something had to be discarded, so the caller can
    /// report it *after* releasing the lock. Never blocks on the worker.
    #[must_use = "a returned count means an entry was dropped and should be reported"]
    pub(crate) fn push(&self, record: Record) -> Option<u64> {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.shutdown {
            // Past the point of no return: the worker is on its way out and would never send this.
            return Some(self.dropped.fetch_add(1, Ordering::Relaxed) + 1);
        }

        let dropped = if state.items.len() >= self.capacity {
            state.items.pop_front();
            Some(self.dropped.fetch_add(1, Ordering::Relaxed) + 1)
        } else {
            None
        };
        state.items.push_back(record);
        drop(state);

        self.work.notify_one();
        dropped
    }

    /// Puts an entry back at the head, after a failed write.
    ///
    /// Go's client loses the in-flight entry in this situation; C++ re-queues it, and so does this.
    /// If the queue has filled up behind it in the meantime the entry is dropped rather than
    /// pushing the queue over capacity.
    pub(crate) fn push_front(&self, record: Record) -> Option<u64> {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.items.len() >= self.capacity {
            return Some(self.dropped.fetch_add(1, Ordering::Relaxed) + 1);
        }
        state.items.push_front(record);
        drop(state);
        self.work.notify_one();
        None
    }

    /// Takes the next entry, waiting up to `timeout` for one to arrive.
    ///
    /// Marks the queue busy when it hands an entry out; the caller must follow up with
    /// [`Queue::finish`] once the entry has been written or re-queued.
    pub(crate) fn pop(&self, timeout: Duration) -> Pop {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.items.is_empty() && !state.shutdown {
            let (guard, _) = self
                .work
                .wait_timeout_while(state, timeout, |s| s.items.is_empty() && !s.shutdown)
                .unwrap_or_else(|e| e.into_inner());
            state = guard;
        }

        // Drain before honouring shutdown, so a flush-on-close still gets to send what is queued.
        if let Some(record) = state.items.pop_front() {
            state.busy = true;
            return Pop::Item(record);
        }
        if state.shutdown {
            Pop::Shutdown
        } else {
            Pop::Idle
        }
    }

    /// Clears the busy flag set by [`Queue::pop`], waking any waiting [`Queue::flush`].
    pub(crate) fn finish(&self) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.busy = false;
        let idle = state.items.is_empty();
        drop(state);
        if idle {
            self.drained.notify_all();
        }
    }

    /// Blocks until the queue is empty and the worker is idle, or `timeout` elapses.
    ///
    /// Returns whether it drained. "Drained" means every entry reached the operating system, not
    /// that the GUI displayed it -- the server never acknowledges anything, so that is the
    /// strongest claim any client of this protocol can make.
    pub(crate) fn flush(&self, timeout: Duration) -> bool {
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let (state, _) = self
            .drained
            .wait_timeout_while(state, timeout, |s| !s.items.is_empty() || s.busy)
            .unwrap_or_else(|e| e.into_inner());
        state.items.is_empty() && !state.busy
    }

    /// Requests shutdown and wakes everybody waiting.
    pub(crate) fn shutdown(&self) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.shutdown = true;
        drop(state);
        self.work.notify_all();
        self.drained.notify_all();
    }

    /// Whether nothing is queued. Does not account for an entry the worker is already holding.
    pub(crate) fn is_empty(&self) -> bool {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .items
            .is_empty()
    }

    pub(crate) fn is_shutdown(&self) -> bool {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .shutdown
    }

    /// Sleeps for `duration` unless shutdown is requested first. Returns `true` if it was.
    ///
    /// Used for the reconnect delay: a plain `thread::sleep` there is what makes the sibling
    /// clients take up to five seconds to shut down.
    pub(crate) fn sleep_or_shutdown(&self, duration: Duration) -> bool {
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let (state, _) = self
            .work
            .wait_timeout_while(state, duration, |s| !s.shutdown)
            .unwrap_or_else(|e| e.into_inner());
        state.shutdown
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::priority::Priority;
    use std::sync::Arc;
    use std::time::Instant;

    fn record(message: &str) -> Record {
        Record::new(Priority::Debug, "app", message)
    }

    fn drain(queue: &Queue) -> Vec<String> {
        let mut out = Vec::new();
        while let Pop::Item(r) = queue.pop(Duration::ZERO) {
            out.push(r.message.clone());
            queue.finish();
        }
        out
    }

    #[test]
    fn keeps_the_newest_when_full() {
        let queue = Queue::new(3);
        for i in 0..5 {
            let _ = queue.push(record(&i.to_string()));
        }
        // 0 and 1 were pushed out by 3 and 4.
        assert_eq!(drain(&queue), ["2", "3", "4"]);
        assert_eq!(queue.dropped_count(), 2);
    }

    #[test]
    fn reports_a_drop_exactly_once_per_discarded_entry() {
        let queue = Queue::new(1);
        assert_eq!(queue.push(record("a")), None);
        assert_eq!(queue.push(record("b")), Some(1));
        assert_eq!(queue.push(record("c")), Some(2));
        assert_eq!(queue.dropped_count(), 2);
    }

    #[test]
    fn push_front_puts_a_failed_write_back_at_the_head() {
        let queue = Queue::new(4);
        let _ = queue.push(record("b"));
        let _ = queue.push(record("c"));
        assert_eq!(queue.push_front(record("a")), None);
        assert_eq!(drain(&queue), ["a", "b", "c"]);
    }

    #[test]
    fn push_front_does_not_exceed_capacity() {
        let queue = Queue::new(2);
        let _ = queue.push(record("a"));
        let _ = queue.push(record("b"));
        assert_eq!(queue.push_front(record("requeued")), Some(1));
        assert_eq!(drain(&queue), ["a", "b"]);
    }

    #[test]
    fn pop_drains_before_reporting_shutdown() {
        let queue = Queue::new(4);
        let _ = queue.push(record("a"));
        queue.shutdown();
        assert!(matches!(queue.pop(Duration::ZERO), Pop::Item(_)));
        queue.finish();
        assert!(matches!(queue.pop(Duration::ZERO), Pop::Shutdown));
    }

    #[test]
    fn pop_reports_idle_when_nothing_arrives() {
        let queue = Queue::new(4);
        assert!(matches!(queue.pop(Duration::from_millis(1)), Pop::Idle));
    }

    #[test]
    fn a_push_after_shutdown_is_counted_not_queued() {
        let queue = Queue::new(4);
        queue.shutdown();
        assert_eq!(queue.push(record("a")), Some(1));
        assert!(matches!(queue.pop(Duration::ZERO), Pop::Shutdown));
    }

    #[test]
    fn flush_waits_for_an_entry_the_worker_is_still_holding() {
        let queue = Arc::new(Queue::new(4));
        let _ = queue.push(record("a"));
        let Pop::Item(_) = queue.pop(Duration::ZERO) else {
            panic!("expected an entry");
        };
        // The queue is empty but the entry has not been written, so flush must not succeed yet.
        assert!(!queue.flush(Duration::from_millis(20)));

        let q = Arc::clone(&queue);
        std::thread::spawn(move || q.finish());
        assert!(queue.flush(Duration::from_secs(5)));
    }

    #[test]
    fn shutdown_interrupts_a_pending_sleep() {
        let queue = Arc::new(Queue::new(4));
        let q = Arc::clone(&queue);
        let start = Instant::now();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(20));
            q.shutdown();
        });
        assert!(queue.sleep_or_shutdown(Duration::from_secs(30)));
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "sleep_or_shutdown waited out the full delay"
        );
    }

    #[test]
    fn a_waiting_pop_wakes_on_a_push() {
        let queue = Arc::new(Queue::new(4));
        let q = Arc::clone(&queue);
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(20));
            let _ = q.push(record("a"));
        });
        // Would return Idle if the push did not wake the waiter.
        assert!(matches!(queue.pop(Duration::from_secs(5)), Pop::Item(_)));
    }
}
