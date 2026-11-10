//! The terminal's always-on and opt-in measurements. Ports of `gridMismatchProbe`
//! and `terminalPanePerf`, and the part of `paintTailProbe` that outlives
//! xterm. All sans-IO: the caller passes `now` and acts on what comes back.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use terminal_engine::GridSize;

/// A disagreement must hold this long to be believed: a drag legitimately
/// disagrees for a debounce plus a host round trip plus a rediscovery.
pub const GRID_MISMATCH_SUSTAIN: Duration = Duration::from_secs(10);
/// tmux gives a split column to the divider, so one cell is a healthy pane.
pub const GRID_MISMATCH_MIN_DELTA: usize = 2;
pub const GRID_MISMATCH_INCIDENT_INTERVAL: Duration = Duration::from_secs(60);

struct PendingMismatch {
    armed_at: Instant,
    tmux: GridSize,
    measured: GridSize,
}

/// A pane still rendering at a grid its own box stopped agreeing with, after
/// the round trip that should have closed the gap has long been answered.
/// Journal-only: a tripwire that healed the state would erase how it got there.
#[derive(Default)]
pub struct GridMismatchProbe {
    pending: HashMap<String, PendingMismatch>,
    last_incident: HashMap<String, Instant>,
}

fn disagrees(tmux: GridSize, measured: GridSize) -> bool {
    tmux.columns.abs_diff(measured.columns) >= GRID_MISMATCH_MIN_DELTA
        || tmux.rows.abs_diff(measured.rows) >= GRID_MISMATCH_MIN_DELTA
}

impl GridMismatchProbe {
    pub fn note_grids(
        &mut self,
        pane_id: &str,
        tmux: GridSize,
        measured: Option<GridSize>,
        now: Instant,
    ) {
        let Some(measured) = measured.filter(|measured| disagrees(tmux, *measured)) else {
            self.pending.remove(pane_id);
            return;
        };
        // One episode however much the numbers move: a dragged pane remeasures
        // every frame, and restarting the clock on each pair would mean a
        // disagreement that never stops never reaches its deadline.
        let pending = self
            .pending
            .entry(pane_id.to_owned())
            .or_insert(PendingMismatch {
                armed_at: now,
                tmux,
                measured,
            });
        pending.tmux = tmux;
        pending.measured = measured;
    }

    pub fn clear(&mut self, pane_id: &str) {
        self.pending.remove(pane_id);
    }

    /// Incidents due at `now`, as `(kind, detail)`.
    pub fn poll(&mut self, now: Instant) -> Vec<Value> {
        let due: Vec<String> = self
            .pending
            .iter()
            .filter(|(_, open)| now.duration_since(open.armed_at) >= GRID_MISMATCH_SUSTAIN)
            .map(|(pane_id, _)| pane_id.clone())
            .collect();
        let mut incidents = Vec::new();
        for pane_id in due {
            // Cleared first: the rate limit may swallow this line, and a
            // swallowed episode must still leave the pane free to arm a new one.
            let Some(open) = self.pending.remove(&pane_id) else {
                continue;
            };
            let recent = self
                .last_incident
                .get(&pane_id)
                .is_some_and(|last| now.duration_since(*last) < GRID_MISMATCH_INCIDENT_INTERVAL);
            if recent {
                continue;
            }
            self.last_incident.insert(pane_id.clone(), now);
            incidents.push(json!({
                "paneId": pane_id,
                "tmuxColumns": open.tmux.columns,
                "tmuxRows": open.tmux.rows,
                "measuredColumns": open.measured.columns,
                "measuredRows": open.measured.rows,
                "msSustained": now.duration_since(open.armed_at).as_millis() as u64,
            }));
        }
        incidents
    }
}

/// How slow a repaint must be before it is worth a journal line: three frames.
pub const PAINT_SLOW_THRESHOLD: Duration = Duration::from_millis(48);
/// A window the OS is not painting clears a higher bar: its frames are throttled
/// or parked, which is not the app being slow.
pub const PAINT_SLOW_HIDDEN_THRESHOLD: Duration = Duration::from_millis(250);
pub const PAINT_SLOW_INCIDENT_INTERVAL: Duration = Duration::from_secs(5);

#[derive(Default)]
struct PaintPane {
    last_record: Option<Instant>,
    suppressed: u64,
}

/// Attributes a slow repaint. What is left of `paintTailProbe` once there is no
/// write queue and no glyph atlas: the main thread was blocked, or the frame
/// clock stopped.
const LONG_STALL: Duration = Duration::from_millis(50);

#[derive(Default)]
pub struct PaintTail {
    panes: HashMap<String, PaintPane>,
    last_frame: Option<Instant>,
    last_stall: Option<(Instant, Duration)>,
}

impl PaintTail {
    /// A gap on the poll timer longer than asked for: the main thread was busy.
    pub fn note_stall(&mut self, stall: Duration, now: Instant) {
        // What the browser would call a long task; timers always overshoot a little.
        if stall >= LONG_STALL {
            self.last_stall = Some((now, stall));
        }
    }

    pub fn note_frame(&mut self, now: Instant) {
        self.last_frame = Some(now);
    }

    /// `waited` is how long a dirty pane waited to be painted. Returns the
    /// `render.paintSlow` record when one is due.
    pub fn note_paint(
        &mut self,
        pane_id: &str,
        waited: Duration,
        window_active: bool,
        now: Instant,
    ) -> Option<Value> {
        let threshold = if window_active {
            PAINT_SLOW_THRESHOLD
        } else {
            PAINT_SLOW_HIDDEN_THRESHOLD
        };
        if waited < threshold {
            return None;
        }
        let pane = self.panes.entry(pane_id.to_owned()).or_default();
        if pane
            .last_record
            .is_some_and(|last| now.duration_since(last) < PAINT_SLOW_INCIDENT_INTERVAL)
        {
            pane.suppressed += 1;
            return None;
        }
        let suppressed = std::mem::take(&mut pane.suppressed);
        pane.last_record = Some(now);
        let ms_since_last_frame = self
            .last_frame
            .map(|frame| now.saturating_duration_since(frame).as_millis() as u64);
        // Only a stall inside the wait can explain it.
        let stall = self
            .last_stall
            .filter(|(at, _)| now.saturating_duration_since(*at) <= waited)
            .map_or(Duration::ZERO, |(_, stall)| stall);
        Some(json!({
            "paneId": pane_id,
            "ms": waited.as_millis() as u64,
            "focused": window_active,
            "msSinceLastFrame": ms_since_last_frame,
            "longStallMs": stall.as_millis() as u64,
            "suppressed": suppressed,
        }))
    }
}

const PANE_PERF_INTERVAL: Duration = Duration::from_secs(2);

/// One pane's rendering work per interval, aggregated so a busy terminal cannot
/// turn the perf log itself into the bottleneck. Opt-in, as the desktop's is.
#[derive(Default)]
pub struct PanePerf {
    started: Option<Instant>,
    render_events: u64,
    rendered_rows: u64,
    reshaped_rows: u64,
}

impl PanePerf {
    /// One frame's rows. Returns a `perf.terminalPane` record once an interval
    /// has passed with work in it.
    pub fn note_paint(
        &mut self,
        pane_id: &str,
        rendered_rows: usize,
        reshaped_rows: usize,
        connection_epoch: u64,
        now: Instant,
    ) -> Option<Value> {
        let started = *self.started.get_or_insert(now);
        self.render_events += 1;
        self.rendered_rows += rendered_rows as u64;
        self.reshaped_rows += reshaped_rows as u64;
        let elapsed = now.duration_since(started);
        if elapsed < PANE_PERF_INTERVAL {
            return None;
        }
        let record = json!({
            "paneId": pane_id,
            "connectionEpoch": connection_epoch,
            "intervalMs": elapsed.as_millis() as u64,
            "renderEvents": self.render_events,
            "renderedRows": self.rendered_rows,
            "reshapedRows": self.reshaped_rows,
        });
        *self = Self {
            started: Some(now),
            ..Self::default()
        };
        Some(record)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grid(columns: usize, rows: usize) -> GridSize {
        GridSize { columns, rows }
    }

    #[test]
    fn a_small_or_resolved_disagreement_never_reports() {
        let mut probe = GridMismatchProbe::default();
        let start = Instant::now();
        probe.note_grids("%1", grid(80, 24), Some(grid(79, 24)), start);
        assert!(probe.poll(start + GRID_MISMATCH_SUSTAIN * 2).is_empty());
        probe.note_grids("%1", grid(80, 24), Some(grid(60, 24)), start);
        probe.note_grids("%1", grid(80, 24), None, start);
        assert!(probe.poll(start + GRID_MISMATCH_SUSTAIN * 2).is_empty());
        probe.note_grids("%1", grid(80, 24), Some(grid(60, 24)), start);
        probe.clear("%1");
        assert!(probe.poll(start + GRID_MISMATCH_SUSTAIN * 2).is_empty());
    }

    #[test]
    fn a_sustained_disagreement_reports_its_latest_numbers_once() {
        let mut probe = GridMismatchProbe::default();
        let start = Instant::now();
        probe.note_grids("%1", grid(80, 24), Some(grid(60, 24)), start);
        let early = start + GRID_MISMATCH_SUSTAIN - Duration::from_millis(1);
        probe.note_grids("%1", grid(80, 24), Some(grid(55, 20)), early);
        assert!(probe.poll(early).is_empty(), "before the deadline");
        let due = start + GRID_MISMATCH_SUSTAIN;
        let incidents = probe.poll(due);
        assert_eq!(incidents.len(), 1);
        assert_eq!(incidents[0]["measuredColumns"], 55);
        assert_eq!(incidents[0]["measuredRows"], 20);
        assert_eq!(incidents[0]["msSustained"], 10_000);
        assert!(probe.poll(due).is_empty(), "one line per episode");
    }

    #[test]
    fn a_later_episode_inside_the_interval_is_swallowed_but_can_rearm() {
        let mut probe = GridMismatchProbe::default();
        let start = Instant::now();
        probe.note_grids("%1", grid(80, 24), Some(grid(60, 24)), start);
        assert_eq!(probe.poll(start + GRID_MISMATCH_SUSTAIN).len(), 1);
        let again = start + GRID_MISMATCH_SUSTAIN + Duration::from_secs(1);
        probe.note_grids("%1", grid(80, 24), Some(grid(60, 24)), again);
        assert!(probe.poll(again + GRID_MISMATCH_SUSTAIN).is_empty());
        probe.note_grids("%1", grid(80, 24), Some(grid(60, 24)), again);
        let later = again + GRID_MISMATCH_INCIDENT_INTERVAL + GRID_MISMATCH_SUSTAIN;
        assert_eq!(probe.poll(later).len(), 1);
    }

    #[test]
    fn a_fast_paint_costs_nothing_and_a_hidden_window_clears_a_higher_bar() {
        let mut tail = PaintTail::default();
        let now = Instant::now();
        assert!(
            tail.note_paint("%1", Duration::from_millis(10), true, now)
                .is_none()
        );
        assert!(
            tail.note_paint("%1", Duration::from_millis(100), false, now)
                .is_none(),
            "a parked window is not the app being slow"
        );
        assert!(
            tail.note_paint("%1", Duration::from_millis(300), false, now)
                .is_some()
        );
    }

    #[test]
    fn a_slow_paint_carries_its_fingerprints_and_rate_limits_per_pane() {
        let mut tail = PaintTail::default();
        let start = Instant::now();
        tail.note_frame(start);
        let later = start + Duration::from_millis(300);
        tail.note_stall(
            Duration::from_millis(120),
            start + Duration::from_millis(150),
        );
        let record = tail
            .note_paint("%1", Duration::from_millis(200), true, later)
            .unwrap();
        assert_eq!(record["ms"], 200);
        assert_eq!(record["msSinceLastFrame"], 300);
        assert_eq!(record["longStallMs"], 120);
        assert!(
            tail.note_paint("%1", Duration::from_millis(200), true, later)
                .is_none()
        );
        assert!(
            tail.note_paint("%2", Duration::from_millis(200), true, later)
                .is_some(),
            "another pane has its own limit"
        );
        let after = later + PAINT_SLOW_INCIDENT_INTERVAL;
        let next = tail
            .note_paint("%1", Duration::from_millis(50), true, after)
            .unwrap();
        assert_eq!(next["suppressed"], 1);
    }

    #[test]
    fn pane_perf_reports_per_interval_and_starts_over() {
        let mut perf = PanePerf::default();
        let start = Instant::now();
        assert!(perf.note_paint("%1", 24, 3, 9, start).is_none());
        let later = start + Duration::from_secs(1);
        assert!(perf.note_paint("%1", 2, 1, 9, later).is_none());
        let end = start + PANE_PERF_INTERVAL;
        let record = perf.note_paint("%1", 1, 0, 9, end).unwrap();
        assert_eq!(record["renderEvents"], 3);
        assert_eq!(record["renderedRows"], 27);
        assert_eq!(record["reshapedRows"], 4);
        assert_eq!(record["intervalMs"], 2_000);
        assert_eq!(record["connectionEpoch"], 9);
        assert!(perf.note_paint("%1", 5, 5, 9, end).is_none());
    }
}
