//! Ports of `echoLagProbe.ts` and `inputLatencyStats.ts`: how long a keystroke
//! took to come back, and where the milliseconds of a keystroke go.
//!
//! Journal-only: nothing here touches input, delivery or rendering. Sans-IO,
//! as the other terminal probes are: the caller passes `now` and acts on what
//! comes back.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui_kit::{App, Global, Task};
use serde::Serialize;
use serde_json::{Map, Value, json};

use crate::connection_ui::backend::TerminalBackend;

/// Above the point where an echo stops feeling instant on a remote link. A
/// measurement campaign attributed the 100 to 300 ms spikes to the agent's own
/// repaint, so only what is worse earns a journal line; every echo still lands
/// in the histograms.
pub const ECHO_LAG_THRESHOLD: Duration = Duration::from_millis(250);

/// A keystroke that has not echoed in five seconds is the lag the user is
/// complaining about. Only a run of keystrokes can expire loudly: one lone key
/// into a password prompt or copy-mode echoes nothing and must not be recorded.
pub const ECHO_TIMEOUT: Duration = Duration::from_secs(5);
pub const ECHO_TIMEOUT_MIN_INPUTS: u32 = 2;

/// A stalled pane produces one line per episode, not one per keystroke.
pub const ECHO_INCIDENT_INTERVAL: Duration = Duration::from_secs(10);

/// How long a physical key vouches for the input that follows it. Mouse
/// reports and focus reports are input too, and are not typing.
pub const ECHO_KEY_RECENCY: Duration = Duration::from_millis(250);

/// At most one measurement per pane per interval reads the link's counters:
/// the baseline is a distribution, not every keystroke.
pub const ECHO_SAMPLE_INTERVAL: Duration = Duration::from_millis(250);

/// One reading of the link reader's cumulative counters.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LinkCounters {
    pub bytes_read: u64,
    pub frames_read: u64,
}

impl LinkCounters {
    /// From the link stats the backend reports, when it counts reads at all.
    pub fn from_stats(stats: &Value) -> Option<Self> {
        Some(Self {
            bytes_read: stats.get("bytesReadTotal")?.as_u64()?,
            frames_read: stats.get("framesReadTotal")?.as_u64()?,
        })
    }
}

/// The host output frame that closed a measurement, in a measured build.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EchoOutput {
    pub sequence: u64,
    pub generation: u64,
    pub connection_epoch: u64,
}

/// What a closed measurement leaves behind.
#[derive(Debug, Default, PartialEq)]
pub struct EchoOutcome {
    /// The round trip, for the `endToEnd` histogram.
    pub sample: Option<Duration>,
    /// The sampled record for the perf log.
    pub echo: Option<Value>,
    /// A journal line, as `(kind, detail)`.
    pub incident: Option<(&'static str, Value)>,
}

struct PendingEcho {
    started: Instant,
    input_count: u32,
    /// The counters at the keystroke, when this measurement was sampled.
    before: Option<LinkCounters>,
}

/// One pane's echo measurement.
#[derive(Default)]
pub struct EchoLagProbe {
    pending: Option<PendingEcho>,
    last_key: Option<Instant>,
    last_incident: Option<Instant>,
    last_sample: Option<Instant>,
}

fn millis(duration: Duration) -> u64 {
    duration.as_millis() as u64
}

impl EchoLagProbe {
    /// A physical key was pressed, which is what makes input real.
    pub fn note_key(&mut self, now: Instant) {
        self.last_key = Some(now);
    }

    /// Input bytes left for the pane. Returns true when they opened a
    /// measurement, which the caller expires after [`ECHO_TIMEOUT`].
    pub fn note_input(
        &mut self,
        now: Instant,
        counters: impl FnOnce() -> Option<LinkCounters>,
    ) -> bool {
        // Bytes sent during a running measurement are part of the same wait.
        if let Some(open) = self.pending.as_mut() {
            open.input_count += 1;
            return false;
        }
        if !self
            .last_key
            .is_some_and(|key| now.saturating_duration_since(key) <= ECHO_KEY_RECENCY)
        {
            return false;
        }
        let sampled = self
            .last_sample
            .is_none_or(|at| now.saturating_duration_since(at) >= ECHO_SAMPLE_INTERVAL);
        let before = sampled.then(counters).flatten();
        if before.is_some() {
            self.last_sample = Some(now);
        }
        self.pending = Some(PendingEcho {
            started: now,
            input_count: 1,
            before,
        });
        true
    }

    /// Output reached the pane.
    pub fn note_output(
        &mut self,
        pane_id: &str,
        now: Instant,
        output: Option<EchoOutput>,
        counters: impl FnOnce() -> Option<LinkCounters>,
    ) -> EchoOutcome {
        let Some(open) = self.pending.take() else {
            return EchoOutcome::default();
        };
        let lag = now.saturating_duration_since(open.started);
        let mut detail = json!({
            "paneId": pane_id,
            "lagMs": millis(lag),
            "inputCount": open.input_count,
        });
        if let Some(output) = output {
            extend(
                &mut detail,
                json!({
                    "outputSequence": output.sequence,
                    "outputGeneration": output.generation,
                    "connectionEpoch": output.connection_epoch,
                }),
            );
        }
        // A measurement that cannot read its counters still reports the lag.
        let echo = open.before.map(|before| {
            if let Some(after) = counters() {
                extend(&mut detail, ahead(before, after));
            }
            detail.clone()
        });
        let incident = (lag > ECHO_LAG_THRESHOLD)
            .then(|| self.report(now, "input.echoLag", detail))
            .flatten();
        EchoOutcome {
            sample: Some(lag),
            echo,
            incident,
        }
    }

    /// The measurement's timer ran out. Returns the journal line, if it earns one.
    pub fn expire(
        &mut self,
        pane_id: &str,
        now: Instant,
        counters: impl FnOnce() -> Option<LinkCounters>,
    ) -> Option<(&'static str, Value)> {
        let open = self.pending.take()?;
        if open.input_count < ECHO_TIMEOUT_MIN_INPUTS {
            return None;
        }
        let mut detail = json!({
            "paneId": pane_id,
            "waitedMs": millis(now.saturating_duration_since(open.started)),
            "inputCount": open.input_count,
        });
        if let Some((before, after)) = open.before.zip(counters()) {
            extend(&mut detail, ahead(before, after));
        }
        self.report(now, "input.echoTimeout", detail)
    }

    pub fn is_measuring(&self) -> bool {
        self.pending.is_some()
    }

    fn report(
        &mut self,
        now: Instant,
        kind: &'static str,
        detail: Value,
    ) -> Option<(&'static str, Value)> {
        if self
            .last_incident
            .is_some_and(|at| now.saturating_duration_since(at) < ECHO_INCIDENT_INTERVAL)
        {
            return None;
        }
        self.last_incident = Some(now);
        Some((kind, detail))
    }
}

/// How much other traffic the echo waited behind.
fn ahead(before: LinkCounters, after: LinkCounters) -> Value {
    json!({
        "bytesAhead": after.bytes_read.saturating_sub(before.bytes_read),
        "framesAhead": after.frames_read.saturating_sub(before.frames_read),
    })
}

/// Merges `more`'s fields into `value`; both are objects.
pub fn extend(value: &mut Value, more: Value) {
    if let (Value::Object(fields), Value::Object(more)) = (value, more) {
        fields.extend(more);
    }
}

/// Upper bounds, in milliseconds, of every latency bucket.
///
/// SHARED WITH RUST: `INPUT_LATENCY_BUCKET_BOUNDS_MS` in
/// `crates/desktop-core/src/connection/dispatch.rs` is the same list in the
/// same order, so the input queue's histogram and these read as one
/// distribution.
pub const INPUT_LATENCY_BUCKET_BOUNDS_MS: [f64; 20] = [
    1.,
    2.,
    4.,
    8.,
    16.,
    24.,
    32.,
    48.,
    64.,
    96.,
    128.,
    192.,
    256.,
    384.,
    512.,
    768.,
    1024.,
    2048.,
    4096.,
    f64::INFINITY,
];

/// The first bucket whose upper bound the value does not exceed.
pub fn bucket_index(ms: f64) -> usize {
    INPUT_LATENCY_BUCKET_BOUNDS_MS
        .iter()
        .position(|bound| ms <= *bound)
        .unwrap_or(INPUT_LATENCY_BUCKET_BOUNDS_MS.len() - 1)
}

/// One aggregation window of one segment, in milliseconds.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize)]
pub struct LatencySnapshot {
    pub count: u64,
    pub p50: f64,
    pub p90: f64,
    pub p99: f64,
    pub max: f64,
}

/// A percentile is the upper bound of the bucket its nearest-rank sample falls
/// in, since a bucket knows how many samples landed in it but not where. In the
/// unbounded top bucket it is the observed maximum, which is exact.
pub fn snapshot_from_buckets(counts: &[u64], max: f64) -> LatencySnapshot {
    let total: u64 = counts.iter().sum();
    if total == 0 {
        return LatencySnapshot::default();
    }
    let percentile = |fraction: f64| {
        let rank = ((fraction * total as f64).ceil() as u64).max(1);
        let mut cumulative = 0;
        for (index, count) in counts.iter().enumerate() {
            cumulative += count;
            if cumulative >= rank {
                let bound = INPUT_LATENCY_BUCKET_BOUNDS_MS[index];
                return if bound.is_finite() { bound } else { max };
            }
        }
        max
    };
    LatencySnapshot {
        count: total,
        p50: percentile(0.5),
        p90: percentile(0.9),
        p99: percentile(0.99),
        max,
    }
}

#[derive(Default)]
pub struct LatencyHistogram {
    counts: [u64; 20],
    max: f64,
}

impl LatencyHistogram {
    pub fn record(&mut self, ms: f64) {
        if !ms.is_finite() || ms < 0. {
            return;
        }
        self.counts[bucket_index(ms)] += 1;
        self.max = self.max.max(ms);
    }

    /// Reads the window and starts a new one.
    pub fn snapshot_and_reset(&mut self) -> LatencySnapshot {
        let snapshot = snapshot_from_buckets(&self.counts, self.max);
        *self = Self::default();
        snapshot
    }
}

/// The segments the app can time by itself. What is left of `EndToEnd` after
/// the other three, the input queue among them, is the remote half.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Segment {
    EndToEnd,
    Send,
    Paint,
}

#[derive(Default)]
pub struct LatencyWindow {
    end_to_end: LatencyHistogram,
    send: LatencyHistogram,
    paint: LatencyHistogram,
}

impl LatencyWindow {
    pub fn sample(&mut self, segment: Segment, duration: Duration) {
        let ms = duration.as_secs_f64() * 1000.;
        match segment {
            Segment::EndToEnd => self.end_to_end.record(ms),
            Segment::Send => self.send.record(ms),
            Segment::Paint => self.paint.record(ms),
        }
    }

    /// The `input.latency` record for this window, or nothing while nobody
    /// typed. `native` drains the input queue's own window, and is not asked
    /// while idle.
    pub fn flush(&mut self, native: impl FnOnce() -> Option<Value>) -> Option<Value> {
        let end_to_end = self.end_to_end.snapshot_and_reset();
        if end_to_end.count == 0 {
            return None;
        }
        let mut record = json!({
            "endToEnd": end_to_end,
            "send": self.send.snapshot_and_reset(),
            "paint": self.paint.snapshot_and_reset(),
        });
        // A native half that arrived malformed is dropped rather than reported
        // as buckets that do not mean what their bounds say.
        let queue = native().and_then(|native| {
            let counts: Vec<u64> = native
                .get("bucketCounts")?
                .as_array()?
                .iter()
                .map(Value::as_u64)
                .collect::<Option<_>>()?;
            let max_ms = native.get("maxMs")?.as_f64()?;
            (counts.len() == INPUT_LATENCY_BUCKET_BOUNDS_MS.len())
                .then(|| snapshot_from_buckets(&counts, max_ms))
        });
        if let Some(queue) = queue {
            extend(&mut record, json!({ "rustQueue": queue }));
        }
        Some(record)
    }
}

/// One aggregated record per minute of typing, and nothing while idle.
pub const INPUT_LATENCY_FLUSH_INTERVAL: Duration = Duration::from_secs(60);

/// The app's latency window: one record describes typing wherever it happened.
#[derive(Clone, Default)]
pub struct InputLatency(Rc<RefCell<LatencyState>>);

#[derive(Default)]
struct LatencyState {
    window: LatencyWindow,
    /// Where the record goes and whose input queue it reads: the client typed
    /// into last.
    client: Option<(Arc<dyn TerminalBackend>, String)>,
    _flush: Option<Task<()>>,
}

impl Global for InputLatency {}

impl InputLatency {
    pub fn global(cx: &mut App) -> Self {
        cx.default_global::<Self>().clone()
    }

    pub fn sample(&self, segment: Segment, duration: Duration) {
        self.0.borrow_mut().window.sample(segment, duration);
    }

    /// Input went to `client_id`; starts the minute timer on first use.
    pub fn note_client(&self, backend: &Arc<dyn TerminalBackend>, client_id: &str, cx: &mut App) {
        let mut state = self.0.borrow_mut();
        if !state
            .client
            .as_ref()
            .is_some_and(|(_, current)| current == client_id)
        {
            state.client = Some((backend.clone(), client_id.to_owned()));
        }
        if state._flush.is_none() {
            state._flush = Some(cx.spawn(async move |cx| {
                loop {
                    cx.background_executor()
                        .timer(INPUT_LATENCY_FLUSH_INTERVAL)
                        .await;
                    cx.update(|cx| Self::global(cx).flush());
                }
            }));
        }
    }

    fn flush(&self) {
        let mut state = self.0.borrow_mut();
        let Some((backend, client_id)) = state.client.clone() else {
            return;
        };
        if let Some(record) = state
            .window
            .flush(|| backend.input_latency_stats(&client_id))
        {
            backend.record_incident("input.latency", record);
        }
    }
}

/// A perf-log line, written only in a measured process.
pub fn record_perf(kind: &str, fields: Value) {
    if !desktop_core::perf_log::sink::perf_log_enabled() {
        return;
    }
    let mut line = Map::new();
    line.insert("t".into(), chrono::Utc::now().timestamp_millis().into());
    line.insert("kind".into(), kind.into());
    if let Value::Object(fields) = fields {
        line.extend(fields);
    }
    let text = Value::Object(line).to_string();
    desktop_core::platform::spawn(async move {
        // A probe that fails is never a reason to disturb the terminal.
        let _ = desktop_core::perf_log::sink::append_perf_log(vec![text]).await;
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    const MS: Duration = Duration::from_millis(1);

    /// A probe with a hand-driven clock, as the TypeScript tests drive theirs.
    struct Harness {
        probe: EchoLagProbe,
        start: Instant,
        clock: Duration,
        incidents: Vec<(&'static str, Value)>,
        samples: Vec<Duration>,
        /// The measurement's timer, as the pane arms it.
        deadline: Option<Duration>,
    }

    impl Harness {
        fn new() -> Self {
            Self {
                probe: EchoLagProbe::default(),
                start: Instant::now(),
                clock: Duration::ZERO,
                incidents: Vec::new(),
                samples: Vec::new(),
                deadline: None,
            }
        }

        fn now(&self) -> Instant {
            self.start + self.clock
        }

        fn advance(&mut self, by: Duration) {
            self.clock += by;
            if self.deadline.is_some_and(|deadline| self.clock >= deadline) {
                self.deadline = None;
                let now = self.now();
                if let Some(incident) = self.probe.expire("%1", now, || None) {
                    self.incidents.push(incident);
                }
            }
        }

        fn key(&mut self) {
            let now = self.now();
            self.probe.note_key(now);
        }

        fn input(&mut self) {
            let now = self.now();
            if self.probe.note_input(now, || None) {
                self.deadline = Some(self.clock + ECHO_TIMEOUT);
            }
        }

        fn typed(&mut self) {
            self.key();
            self.input();
        }

        fn output(&mut self) {
            let now = self.now();
            let outcome = self.probe.note_output("%1", now, None, || None);
            if outcome.sample.is_some() {
                self.deadline = None;
            }
            self.samples.extend(outcome.sample);
            self.incidents.extend(outcome.incident);
        }
    }

    fn lag(ms: u64, inputs: u32) -> (&'static str, Value) {
        (
            "input.echoLag",
            json!({ "paneId": "%1", "lagMs": ms, "inputCount": inputs }),
        )
    }

    #[test]
    fn records_the_echo_that_came_back_too_late() {
        let mut probe = Harness::new();
        probe.typed();
        probe.advance(ECHO_LAG_THRESHOLD + 120 * MS);
        probe.output();
        assert_eq!(probe.incidents, [lag(370, 1)]);
    }

    #[test]
    fn counts_every_keystroke_typed_into_one_wait() {
        let mut probe = Harness::new();
        probe.typed();
        probe.advance(50 * MS);
        probe.typed();
        probe.advance(ECHO_LAG_THRESHOLD);
        probe.output();
        assert_eq!(probe.incidents, [lag(300, 2)]);
    }

    #[test]
    fn an_echo_in_time_is_silent_and_closes_the_measurement() {
        let mut probe = Harness::new();
        probe.typed();
        probe.advance(ECHO_LAG_THRESHOLD);
        probe.output();
        probe.advance(ECHO_TIMEOUT * 2);
        assert!(probe.incidents.is_empty());
        assert!(!probe.probe.is_measuring());
    }

    #[test]
    fn output_nobody_typed_for_is_ignored() {
        let mut probe = Harness::new();
        probe.advance(10_000 * MS);
        probe.output();
        assert!(probe.incidents.is_empty());
        assert!(probe.samples.is_empty());
    }

    #[test]
    fn a_run_of_keystrokes_that_never_echoed_times_out_once() {
        let mut probe = Harness::new();
        probe.typed();
        probe.typed();
        probe.advance(ECHO_TIMEOUT);
        assert_eq!(
            probe.incidents,
            [(
                "input.echoTimeout",
                json!({ "paneId": "%1", "waitedMs": 5000, "inputCount": 2 })
            )]
        );
        // The measurement is gone: a late echo must not resolve it again.
        probe.output();
        assert_eq!(probe.incidents.len(), 1);
        assert!(
            probe.samples.is_empty(),
            "an expired wait has no round trip"
        );
    }

    #[test]
    fn a_lone_keystroke_expires_silently() {
        let mut probe = Harness::new();
        probe.typed();
        probe.advance(ECHO_TIMEOUT * 2);
        assert!(probe.incidents.is_empty());
    }

    #[test]
    fn one_incident_per_pane_per_rate_limit_window() {
        let mut probe = Harness::new();
        probe.typed();
        probe.advance(ECHO_LAG_THRESHOLD + MS);
        probe.output();
        probe.typed();
        probe.advance(ECHO_LAG_THRESHOLD + MS);
        probe.output();
        assert_eq!(probe.incidents.len(), 1);
        probe.advance(ECHO_INCIDENT_INTERVAL);
        probe.typed();
        probe.advance(ECHO_LAG_THRESHOLD + MS);
        probe.output();
        assert_eq!(probe.incidents.len(), 2);
        assert_eq!(probe.incidents[1].0, "input.echoLag");
        // Every echo is still a sample.
        assert_eq!(probe.samples.len(), 3);
    }

    #[test]
    fn input_no_key_produced_starts_nothing() {
        let mut probe = Harness::new();
        // Mouse and focus reports are input the user did not type.
        probe.input();
        probe.input();
        probe.input();
        assert!(probe.deadline.is_none());
        probe.advance(ECHO_TIMEOUT * 2);
        assert!(probe.incidents.is_empty());
    }

    #[test]
    fn a_key_vouches_for_input_only_while_recent() {
        let mut probe = Harness::new();
        probe.key();
        probe.advance(ECHO_KEY_RECENCY);
        probe.input();
        probe.advance(ECHO_LAG_THRESHOLD + MS);
        probe.output();
        assert_eq!(probe.incidents, [lag(251, 1)]);

        let mut stale = Harness::new();
        stale.key();
        stale.advance(ECHO_KEY_RECENCY + MS);
        stale.input();
        assert!(stale.deadline.is_none());
    }

    #[test]
    fn other_input_counts_into_a_wait_a_key_opened() {
        let mut probe = Harness::new();
        probe.typed();
        probe.advance(1_000 * MS);
        probe.input();
        probe.input();
        probe.advance(ECHO_TIMEOUT - 1_000 * MS);
        assert_eq!(
            probe.incidents,
            [(
                "input.echoTimeout",
                json!({ "paneId": "%1", "waitedMs": 5000, "inputCount": 3 })
            )]
        );
    }

    #[test]
    fn every_echo_is_sampled_fast_or_slow() {
        let mut probe = Harness::new();
        probe.typed();
        probe.advance(12 * MS);
        probe.output();
        probe.typed();
        probe.advance(ECHO_LAG_THRESHOLD + 100 * MS);
        probe.output();
        assert_eq!(probe.samples, [12 * MS, ECHO_LAG_THRESHOLD + 100 * MS]);
        assert_eq!(probe.incidents.len(), 1);
    }

    #[test]
    fn reports_the_traffic_an_echo_waited_behind_at_most_once_per_interval() {
        let start = Instant::now();
        let mut probe = EchoLagProbe::default();
        let mut read = LinkCounters {
            bytes_read: 0,
            frames_read: 0,
        };
        let reads = std::cell::Cell::new(0);
        let mut sample = || {
            reads.set(reads.get() + 1);
            read.bytes_read += 1_000;
            read.frames_read += 2;
            Some(read)
        };
        let output = EchoOutput {
            sequence: 81,
            generation: 144,
            connection_epoch: 23,
        };

        probe.note_key(start);
        assert!(probe.note_input(start, &mut sample));
        let at = start + ECHO_LAG_THRESHOLD + 50 * MS;
        let outcome = probe.note_output("%1", at, Some(output), &mut sample);
        let echo = outcome.echo.expect("a sampled echo");
        for (field, want) in [
            ("lagMs", 300),
            ("bytesAhead", 1_000),
            ("framesAhead", 2),
            ("outputSequence", 81),
            ("outputGeneration", 144),
            ("connectionEpoch", 23),
        ] {
            assert_eq!(echo[field], want, "{field}");
        }
        // The outlier line carries the same evidence as the perf record.
        let (kind, incident) = outcome.incident.expect("an outlier");
        assert_eq!(kind, "input.echoLag");
        assert_eq!(incident["bytesAhead"], 1_000);
        assert_eq!(incident["outputSequence"], 81);

        // The interval has elapsed, so this one is sampled too.
        probe.note_key(at);
        probe.note_input(at, &mut sample);
        let outcome = probe.note_output("%1", at + 10 * MS, None, &mut sample);
        assert!(outcome.echo.is_some());
        assert_eq!(reads.get(), 4);

        // Inside the interval: still measured, but no counters and no record.
        let soon = at + 20 * MS;
        probe.note_key(soon);
        probe.note_input(soon, &mut sample);
        let outcome = probe.note_output("%1", soon + 10 * MS, None, &mut sample);
        assert!(outcome.echo.is_none());
        assert_eq!(outcome.sample, Some(10 * MS));
        assert_eq!(reads.get(), 4);
    }

    #[test]
    fn an_echo_without_counters_writes_no_perf_record() {
        let start = Instant::now();
        let mut probe = EchoLagProbe::default();
        probe.note_key(start);
        probe.note_input(start, || None);
        let outcome = probe.note_output("%1", start + 20 * MS, None, || None);
        assert_eq!(outcome.echo, None);
        assert_eq!(outcome.sample, Some(20 * MS));
    }

    #[test]
    fn buckets_take_the_first_bound_a_value_does_not_exceed() {
        assert_eq!(bucket_index(0.), 0);
        assert_eq!(bucket_index(1.), 0);
        assert_eq!(bucket_index(1.5), 1);
        assert_eq!(bucket_index(2.), 1);
        assert_eq!(bucket_index(2.1), 2);
        assert_eq!(bucket_index(4096.), 18);
        assert_eq!(bucket_index(4097.), 19);
        assert_eq!(bucket_index(1e9), 19);
    }

    fn snapshot(count: u64, p50: f64, p90: f64, p99: f64, max: f64) -> LatencySnapshot {
        LatencySnapshot {
            count,
            p50,
            p90,
            p99,
            max,
        }
    }

    #[test]
    fn a_histogram_reports_bucket_bounds_and_the_true_max() {
        let mut histogram = LatencyHistogram::default();
        assert_eq!(histogram.snapshot_and_reset(), LatencySnapshot::default());
        for _ in 0..90 {
            histogram.record(5.);
        }
        for _ in 0..9 {
            histogram.record(50.);
        }
        histogram.record(300.);
        assert_eq!(
            histogram.snapshot_and_reset(),
            snapshot(100, 8., 8., 64., 300.)
        );
        // A new window after every snapshot.
        assert_eq!(histogram.snapshot_and_reset(), LatencySnapshot::default());

        histogram.record(9_000.);
        assert_eq!(
            histogram.snapshot_and_reset(),
            snapshot(1, 9_000., 9_000., 9_000., 9_000.)
        );
        for value in [f64::NAN, f64::INFINITY, -1.] {
            histogram.record(value);
        }
        assert_eq!(histogram.snapshot_and_reset().count, 0);
    }

    #[test]
    fn native_bucket_counts_read_with_the_same_bounds() {
        let mut counts = [0; 20];
        counts[0] = 3;
        counts[4] = 1;
        assert_eq!(
            snapshot_from_buckets(&counts, 12.),
            snapshot(4, 1., 16., 16., 12.)
        );
    }

    #[test]
    fn a_window_flushes_one_record_with_every_segment() {
        let mut window = LatencyWindow::default();
        window.sample(Segment::EndToEnd, 300 * MS);
        window.sample(Segment::Send, 3 * MS);
        window.sample(Segment::Paint, 6 * MS);
        let mut native_counts = vec![0; 20];
        native_counts[1] = 2;
        let record = window
            .flush(|| Some(json!({ "bucketCounts": native_counts, "maxMs": 2 })))
            .expect("a record");
        let read = |segment: &str| -> LatencySnapshot {
            let fields = &record[segment];
            snapshot(
                fields["count"].as_u64().unwrap(),
                fields["p50"].as_f64().unwrap(),
                fields["p90"].as_f64().unwrap(),
                fields["p99"].as_f64().unwrap(),
                fields["max"].as_f64().unwrap(),
            )
        };
        assert_eq!(read("endToEnd"), snapshot(1, 384., 384., 384., 300.));
        assert_eq!(read("send"), snapshot(1, 4., 4., 4., 3.));
        assert_eq!(read("paint"), snapshot(1, 8., 8., 8., 6.));
        assert_eq!(read("rustQueue"), snapshot(2, 2., 2., 2., 2.));
    }

    #[test]
    fn an_idle_window_records_nothing_and_asks_the_native_side_nothing() {
        let mut window = LatencyWindow::default();
        // A paint sample alone is not a keystroke.
        window.sample(Segment::Paint, 9 * MS);
        let mut asked = false;
        assert_eq!(
            window.flush(|| {
                asked = true;
                None
            }),
            None
        );
        assert!(!asked);
    }

    #[test]
    fn a_missing_or_malformed_native_half_leaves_the_frontend_half() {
        let mut window = LatencyWindow::default();
        window.sample(Segment::EndToEnd, 20 * MS);
        let record = window.flush(|| None).expect("a record");
        assert!(record.get("rustQueue").is_none());
        window.sample(Segment::EndToEnd, 20 * MS);
        let record = window
            .flush(|| Some(json!({ "bucketCounts": [1, 2], "maxMs": 2 })))
            .expect("a record");
        assert!(record.get("rustQueue").is_none());
    }

    #[test]
    fn each_flush_starts_a_fresh_window() {
        let mut window = LatencyWindow::default();
        window.sample(Segment::EndToEnd, 20 * MS);
        window.sample(Segment::Send, 20 * MS);
        window.flush(|| None).expect("a record");
        window.sample(Segment::EndToEnd, 1_000 * MS);
        let record = window.flush(|| None).expect("a record");
        assert_eq!(record["endToEnd"]["count"], 1);
        assert_eq!(record["endToEnd"]["max"], 1_000.);
        assert_eq!(record["send"]["count"], 0);
    }
}
