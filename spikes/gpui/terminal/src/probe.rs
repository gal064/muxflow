//! Latency instrumentation shared by the feed thread and the UI.
//!
//! Leg (a) matches the app's `echoLagProbe`: key dispatch to output delivered to
//! the pane. Leg (b) continues to the frame presented from a grid state that
//! contains the echo. See REPORT.md for exactly where each timestamp sits.

use std::sync::Mutex;
use std::time::Instant;

/// One completed keystroke measurement, in microseconds.
#[derive(Clone, Copy, Debug)]
pub struct KeySample {
    /// Main-thread wait between the driver deciding to press and the input
    /// handler running. Not part of either leg; reported separately because it
    /// is the part of "typing during flood" a main-thread frame can block.
    pub dispatch_wait_us: u64,
    /// t0 to the next `%output` record for the pane (exactly `echoLagProbe`).
    pub next_output_us: u64,
    /// t0 to the first `%output` containing the typed byte.
    pub marker_output_us: u64,
    /// Echo applied to the term until GPUI started building the frame that
    /// shows it: the wait for the next frame tick. Depends on the display's
    /// refresh rate, so it is reported separately from the fixed costs.
    pub frame_wait_us: u64,
    /// t0 to `paint` of the frame built from a grid containing the echo.
    pub frame_paint_us: u64,
    /// t0 to `Window::present` returning for that frame.
    pub frame_present_us: u64,
    /// The typed glyph was found in the grid at paint.
    pub verified: bool,
}

#[derive(Default)]
struct Pending {
    paused: bool,
    pane: String,
    marker: u8,
    expect_at_cursor: bool,
    intended: Option<Instant>,
    t0: Option<Instant>,
    next_output: Option<Instant>,
    marker_output: Option<Instant>,
    awaiting_generation: bool,
    echo_generation: Option<u64>,
    echo_applied: Option<Instant>,
    frame_begin: Option<Instant>,
    painted: Option<Instant>,
    verified: bool,
    completed: bool,
}

#[derive(Default)]
pub struct Probe {
    pending: Mutex<Option<Pending>>,
    samples: Mutex<Vec<KeySample>>,
    timeouts: Mutex<u64>,
    timeouts_after_pause: Mutex<u64>,
    last_dispatch: Mutex<Option<Instant>>,
}

impl Probe {
    /// Start a measurement. `expect_at_cursor` selects the strict glyph check
    /// used for a quiet pane (the cell left of the cursor) versus "anywhere on
    /// the screen" for a flooded pane where the echo can scroll in the same
    /// frame it arrives.
    pub fn arm(&self, pane: &str, marker: u8, expect_at_cursor: bool, intended: Instant) {
        *self.pending.lock().unwrap() = Some(Pending {
            pane: pane.to_string(),
            marker,
            expect_at_cursor,
            intended: Some(intended),
            ..Pending::default()
        });
    }

    /// Called in the input handler, on the main thread, just before the bytes
    /// go to the writer thread.
    pub fn note_dispatch(&self, pane: &str) {
        if let Some(pending) = self.pending.lock().unwrap().as_mut()
            && pending.pane == pane
            && pending.t0.is_none()
        {
            let now = Instant::now();
            pending.t0 = Some(now);
            *self.last_dispatch.lock().unwrap() = Some(now);
        }
    }

    /// When the most recent measured key was dispatched.
    pub fn last_dispatch(&self) -> Option<Instant> {
        *self.last_dispatch.lock().unwrap()
    }

    /// Feed thread, before applying the record to the term.
    pub fn on_output(&self, pane: &str, data: &[u8]) {
        let mut guard = self.pending.lock().unwrap();
        let Some(pending) = guard.as_mut() else {
            return;
        };
        if pending.pane != pane || pending.t0.is_none() {
            return;
        }
        let now = Instant::now();
        if pending.next_output.is_none() {
            pending.next_output = Some(now);
        }
        if pending.marker_output.is_none() && data.contains(&pending.marker) {
            pending.marker_output = Some(now);
            pending.awaiting_generation = true;
        }
    }

    /// Feed thread, right after the record was applied; `generation` is the
    /// pane's post-apply generation.
    pub fn after_feed(&self, pane: &str, generation: u64) {
        let mut guard = self.pending.lock().unwrap();
        let Some(pending) = guard.as_mut() else {
            return;
        };
        if pending.pane == pane && pending.awaiting_generation {
            pending.awaiting_generation = false;
            pending.echo_generation = Some(generation);
            pending.echo_applied = Some(Instant::now());
        }
    }

    /// In `TerminalElement::prepaint`, once this pane's grid is snapshotted at
    /// `generation`. Returns the byte to verify in the grid, if this is the
    /// first frame that contains the echo.
    pub fn on_paint(
        &self,
        pane: &str,
        generation: u64,
        frame_begin: Option<Instant>,
    ) -> Option<(u8, bool)> {
        let mut guard = self.pending.lock().unwrap();
        let pending = guard.as_mut()?;
        if pending.pane != pane || pending.painted.is_some() || pending.completed {
            return None;
        }
        if pending.echo_generation.is_some_and(|echo| generation >= echo) {
            pending.painted = Some(Instant::now());
            pending.frame_begin = frame_begin;
            return Some((pending.marker, pending.expect_at_cursor));
        }
        None
    }

    pub fn mark_verified(&self, verified: bool) {
        if let Some(pending) = self.pending.lock().unwrap().as_mut() {
            pending.verified = verified;
        }
    }

    /// After `Window::present` returned (from a deferred effect queued during
    /// paint). Completes the measurement if its frame was this one.
    pub fn on_present(&self, presented: Instant) -> bool {
        let mut guard = self.pending.lock().unwrap();
        let Some(pending) = guard.as_mut() else {
            return false;
        };
        let Some(painted) = pending.painted else {
            return false;
        };
        if pending.completed {
            return false;
        }
        pending.completed = true;
        let t0 = pending.t0.expect("painted implies dispatched");
        let us = |to: Instant| to.saturating_duration_since(t0).as_micros() as u64;
        let sample = KeySample {
            dispatch_wait_us: t0
                .saturating_duration_since(pending.intended.unwrap_or(t0))
                .as_micros() as u64,
            next_output_us: pending.next_output.map(us).unwrap_or(0),
            marker_output_us: pending.marker_output.map(us).unwrap_or(0),
            frame_wait_us: match (pending.echo_applied, pending.frame_begin) {
                (Some(applied), Some(begin)) => begin.saturating_duration_since(applied).as_micros() as u64,
                _ => 0,
            },
            frame_paint_us: us(painted),
            frame_present_us: us(presented),
            verified: pending.verified,
        };
        drop(guard);
        self.samples.lock().unwrap().push(sample);
        true
    }

    pub fn is_completed(&self) -> bool {
        self.pending
            .lock()
            .unwrap()
            .as_ref()
            .is_none_or(|p| p.completed)
    }

    /// tmux paused `pane`: anything it had queued for this client is discarded,
    /// including an echo still in that queue.
    pub fn on_pause(&self, pane: &str) {
        if let Some(pending) = self.pending.lock().unwrap().as_mut() {
            if pending.pane == pane {
                pending.paused = true;
            }
        }
    }

    pub fn time_out_pending(&self) {
        *self.timeouts.lock().unwrap() += 1;
        let pending = self.pending.lock().unwrap().take();
        if pending.is_some_and(|p| p.paused) {
            *self.timeouts_after_pause.lock().unwrap() += 1;
        }
    }

    pub fn timeouts_after_pause(&self) -> u64 {
        *self.timeouts_after_pause.lock().unwrap()
    }

    pub fn timeouts(&self) -> u64 {
        *self.timeouts.lock().unwrap()
    }

    pub fn take_samples(&self) -> Vec<KeySample> {
        std::mem::take(&mut self.samples.lock().unwrap())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn next_output_closes_on_any_output_but_marker_waits_for_the_byte() {
        let probe = Probe::default();
        probe.arm("%1", b'z', true, Instant::now());
        probe.note_dispatch("%1");
        probe.on_output("%2", b"zzz"); // other pane: ignored
        probe.on_output("%1", b"unrelated noise");
        probe.after_feed("%1", 1);
        assert!(probe.on_paint("%1", 1, None).is_none(), "echo not seen yet");
        probe.on_output("%1", b"z");
        probe.after_feed("%1", 2);
        assert!(probe.on_paint("%1", 1, None).is_none(), "older snapshot");
        assert_eq!(probe.on_paint("%1", 2, None), Some((b'z', true)));
        probe.mark_verified(true);
        assert!(probe.on_present(Instant::now()));
        let samples = probe.take_samples();
        assert_eq!(samples.len(), 1);
        assert!(samples[0].verified);
    }

    #[test]
    fn output_before_dispatch_is_ignored() {
        let probe = Probe::default();
        probe.arm("%1", b'z', true, Instant::now());
        probe.on_output("%1", b"z");
        probe.after_feed("%1", 1);
        probe.note_dispatch("%1");
        assert!(probe.on_paint("%1", 5, None).is_none());
    }

    #[test]
    fn present_only_completes_after_the_echo_frame_painted() {
        let probe = Probe::default();
        probe.arm("%1", b'z', false, Instant::now());
        probe.note_dispatch("%1");
        assert!(!probe.on_present(Instant::now()));
        probe.on_output("%1", b"z");
        probe.after_feed("%1", 3);
        assert_eq!(probe.on_paint("%1", 3, None), Some((b'z', false)));
        assert!(probe.on_present(Instant::now()));
        assert!(!probe.on_present(Instant::now()), "completes once");
        assert!(probe.is_completed());
    }
}
