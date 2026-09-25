//! Turns pane output into a topology reconciliation trigger.
//!
//! tmux notifies the control client about *structural* change, and those
//! notifications are what normally drive [`TopologySignal::mark_dirty`]. It
//! stays silent for two changes the desktop renders continuously: an automatic
//! (title-driven) window rename, and a pane's working directory moving. A bare
//! `cd` in an otherwise quiet pane produces no control-mode record at all, so
//! the Explorer (which follows the active pane's cwd) and the workspace tabs
//! (which show window names) stayed stale until some unrelated notification
//! arrived or the thirty-second safety tick fired.
//!
//! What a `cd` *does* always produce is a new prompt, and what a retitle always
//! follows is a command — both are pane output. This unit converts that
//! firehose into a bounded trickle of dirty marks: leading edge so a quiet
//! pane's single line reconciles on the next actor wakeup, trailing edge so an
//! isolated second line inside the window is never dropped on the floor until
//! the safety tick.
//!
//! # Expected steady state
//!
//! * Agents retitling their windows about once a second: at most ~2 dirty marks
//!   per [`OUTPUT_DIRTY_WINDOW`], i.e. ~2 reconciles/second. A reconcile is one
//!   `tmux list-*` discovery on a blocking thread — a few milliseconds of CPU.
//! * Snapshot pushes stay driven by the actor's compare-before-push, so the
//!   client sees a `TopologySnapshot` only when the topology really changed
//!   (~1/second during that churn), not once per dirty mark.
//! * A bare `cd` in a quiet pane takes the leading edge: the mark happens on
//!   the reader thread that delivered the prompt bytes, so the snapshot reaches
//!   the client well under a second.
//! * A pane streaming megabytes costs one acquire load and one comparison per
//!   record — nanoseconds — and produces the same ~2 marks per window as a pane
//!   printing two lines.
//! * The 30 s safety tick remains the backstop; nothing here replaces it.
//!
//! This trigger never emits a `TopologyDirty` host event. It only wakes the
//! daemon-side actor: the desktop journals `TopologyDirty` and flashes a status
//! message for it, and output is far too common to narrate.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use tokio::runtime::Handle;

use super::topology::TopologySignal;

/// How long one dirty mark suppresses the next.
///
/// Long enough that a pane printing continuously cannot turn every read into a
/// tmux discovery, short enough that a rename the user just caused is on screen
/// before they look for it.
const OUTPUT_DIRTY_WINDOW: Duration = Duration::from_millis(500);

/// Sentinel for "no mark has ever been made", so the very first record after a
/// connection opens takes the leading edge instead of being suppressed by an
/// implicit mark at construction time.
const NEVER_FIRED: u64 = u64::MAX;

/// Marks the topology signal dirty on pane output, debounced leading+trailing.
///
/// Cloneable and cheap to clone; every terminal attachment's reader thread
/// holds one. The default value is inert, which is what test harnesses and any
/// terminal client without a topology actor behind it construct.
#[derive(Clone, Default)]
pub(super) struct TopologyOutputTrigger {
    inner: Option<Arc<TriggerState>>,
}

impl TopologyOutputTrigger {
    pub(super) fn new(signal: TopologySignal, handle: Handle) -> Self {
        Self::with_window(signal, handle, OUTPUT_DIRTY_WINDOW)
    }

    fn with_window(signal: TopologySignal, handle: Handle, window: Duration) -> Self {
        Self {
            inner: Some(Arc::new(TriggerState {
                signal,
                handle,
                window_nanos: window.as_nanos() as u64,
                origin: Instant::now(),
                last_fired: AtomicU64::new(NEVER_FIRED),
                trailing_scheduled: AtomicBool::new(false),
            })),
        }
    }

    /// Records that a pane delivered output.
    ///
    /// Called once per delivered record from every control-stream reader
    /// thread, and deliberately *before* the emission-order fence is taken:
    /// this must never add work under that lock. The common case is a load, a
    /// subtraction and a branch.
    pub(super) fn note_output(&self) {
        if let Some(state) = &self.inner {
            state.note_output();
        }
    }
}

struct TriggerState {
    signal: TopologySignal,
    /// The connection's runtime, captured where one is current. Reader threads
    /// are plain `std` threads with no runtime context of their own, so a
    /// trailing mark has to be spawned through an explicit handle.
    handle: Handle,
    window_nanos: u64,
    origin: Instant,
    /// Nanoseconds since `origin` at which the last mark was made, or
    /// [`NEVER_FIRED`].
    last_fired: AtomicU64,
    /// Whether a trailing mark is already owed. Exactly one record inside a
    /// suppressed window wins this flag and spawns the deferred mark.
    trailing_scheduled: AtomicBool,
}

impl TriggerState {
    fn note_output(self: &Arc<Self>) {
        let now = self.elapsed_nanos();
        let remaining = loop {
            let last = self.last_fired.load(Ordering::Acquire);
            if last != NEVER_FIRED {
                // A record whose clock read predates a concurrent winner's is
                // treated as landing at the window's start: a whole window is
                // the conservative wait, and waiting too long only delays a
                // mark, never loses one.
                let since = now.saturating_sub(last);
                if since < self.window_nanos {
                    break self.window_nanos - since;
                }
            }
            if self
                .last_fired
                .compare_exchange_weak(last, now, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                self.signal.mark_dirty();
                return;
            }
        };
        if self.trailing_scheduled.swap(true, Ordering::AcqRel) {
            // Someone else already owes the mark that covers this record.
            return;
        }
        let state = Arc::clone(self);
        let delay = Duration::from_nanos(remaining);
        self.handle.spawn(async move {
            tokio::time::sleep(delay).await;
            state.fire_trailing();
        });
    }

    fn fire_trailing(&self) {
        // Order matters. The window is restarted first so a record racing this
        // function either lands inside the fresh window or legitimately takes a
        // new leading edge. The owed-mark slot is then released *before* the
        // mark itself, because a record that arrives between the release and
        // the mark schedules its own trailing, whereas a record arriving
        // between the mark and a later release would find the slot taken and
        // have nothing left to cover it.
        self.last_fired
            .store(self.elapsed_nanos(), Ordering::Release);
        self.trailing_scheduled.store(false, Ordering::Release);
        self.signal.mark_dirty();
    }

    fn elapsed_nanos(&self) -> u64 {
        self.origin.elapsed().as_nanos() as u64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_WINDOW: Duration = Duration::from_millis(60);

    fn trigger(signal: &TopologySignal) -> TopologyOutputTrigger {
        TopologyOutputTrigger::with_window(signal.clone(), Handle::current(), TEST_WINDOW)
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn output_from_a_quiet_pane_marks_dirty_immediately() {
        let signal = TopologySignal::default();
        let trigger = trigger(&signal);

        trigger.note_output();

        assert_eq!(
            signal.current_epoch(),
            1,
            "a quiet pane's first line must not wait for a timer"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn output_inside_the_window_is_suppressed() {
        let signal = TopologySignal::default();
        let trigger = trigger(&signal);

        for _ in 0..64 {
            trigger.note_output();
        }

        assert_eq!(
            signal.current_epoch(),
            1,
            "only the leading edge may mark dirty inside one window"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_single_suppressed_record_still_marks_dirty_exactly_once() {
        let signal = TopologySignal::default();
        let trigger = trigger(&signal);

        // The leading edge, then one isolated record — the `cd` whose prompt
        // arrived just after some unrelated byte.
        trigger.note_output();
        trigger.note_output();
        assert_eq!(signal.current_epoch(), 1);

        tokio::time::sleep(TEST_WINDOW * 3).await;
        assert_eq!(
            signal.current_epoch(),
            2,
            "the suppressed record must reach the actor at the window's end"
        );

        tokio::time::sleep(TEST_WINDOW * 3).await;
        assert_eq!(
            signal.current_epoch(),
            2,
            "the trailing mark must fire once, not repeat"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_hammering_stays_within_about_two_marks_per_window() {
        let signal = TopologySignal::default();
        let trigger = trigger(&signal);
        let started = Instant::now();

        let hammering: Vec<_> = (0..8)
            .map(|_| {
                let trigger = trigger.clone();
                std::thread::spawn(move || {
                    let deadline = Instant::now() + TEST_WINDOW * 3;
                    while Instant::now() < deadline {
                        trigger.note_output();
                    }
                })
            })
            .collect();
        for thread in hammering {
            thread.join().unwrap();
        }
        tokio::time::sleep(TEST_WINDOW * 2).await;

        let marks = signal.current_epoch();
        let windows = started.elapsed().as_nanos() / TEST_WINDOW.as_nanos();
        assert!(
            marks >= 2,
            "sustained output must keep reconciliation alive, saw {marks} marks"
        );
        // The module's contract is at most ~2 marks per window: a leading edge
        // and one trailing mark. On a starved runtime the trailing mark can land
        // just after the next leading edge, so the bound is the contract's, not
        // one per window.
        assert!(
            u128::from(marks) <= 2 * windows + 2,
            "millions of records over {windows} windows produced {marks} marks"
        );
    }
}
