//! The in-app latency probe. Port of `perf/probe.ts`, `paintTicket.ts` and the
//! opt-in in `bootstrap.ts`.
//!
//! Several budgets (keystroke to glyph, action to a painted pane, a directory
//! expanded) only exist inside the app. This records them and appends them to
//! the file `ADE_PERF_LOG` names, in the schema `summarize-perf-log.py` reads:
//! one JSON object per line, a sample as `{t, name, ms}`, a structured record
//! as `{t, kind, ...}`, rolling `operations` and `summary` aggregates, and a
//! `recordId` on every line so a retried batch can be deduplicated.
//!
//! Inert unless the process was started with `ADE_PERF_LOG` (and, in a release
//! build, compiled with the `perf-log` feature): desktop-core's sink decides,
//! and nothing here turns itself on. Nothing here changes what the user sees.
//!
//! One process-wide probe, as the TS module was: the pane that paints and the
//! action that waits for it are in different parts of the app. Its state sits
//! behind a mutex so the flush can run on desktop-core's runtime without ever
//! waking a GPUI task.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

use serde_json::{Map, Value, json};

/// Per-metric sample cap. Old samples are dropped, never the newest ones.
const MAX_SAMPLES: usize = 1_024;
const FLUSH_INTERVAL: Duration = Duration::from_secs(2);
/// Room left in a line for the `recordId` the sink stamps on it.
const RECORD_ID_ROOM: usize = 80;
const MAX_PENDING_SPANS: usize = 256;
const SPAN_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_RECENT_PAINTS: usize = 256;

/// What the native append accepts, from the contract both sides compile in.
struct Contract {
    max_lines_per_append: usize,
    max_line_bytes: usize,
}

fn contract() -> &'static Contract {
    static CONTRACT: OnceLock<Contract> = OnceLock::new();
    CONTRACT.get_or_init(|| {
        let value: Value =
            serde_json::from_str(include_str!("../perf-log-contract.json"))
                .expect("perf-log-contract.json must remain valid");
        let number = |key: &str| value[key].as_u64().expect("contract number") as usize;
        Contract {
            max_lines_per_append: number("maxLinesPerAppend"),
            max_line_bytes: number("maxLineBytes"),
        }
    })
}

/// User interactions that end when the pane they produced first paints.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PaneSpan {
    CreateTab,
    CreateWorkspace,
    PaneSplit,
    WindowSwitch,
}

impl PaneSpan {
    fn name(self) -> &'static str {
        match self {
            Self::CreateTab => "create.tab",
            Self::CreateWorkspace => "create.workspace",
            Self::PaneSplit => "pane.split",
            Self::WindowSwitch => "window.switch",
        }
    }
}

struct OpenSpan {
    name: PaneSpan,
    scope: String,
    started: Instant,
    pane_id: Option<String>,
}

/// The probe's state, sans IO: callers pass the clocks, and the sink is
/// whatever takes [`Probe::take_batch`] and reports back.
pub struct Probe {
    record_prefix: String,
    next_record: u64,
    samples: BTreeMap<String, VecDeque<f64>>,
    counters: BTreeMap<String, i64>,
    high_water: BTreeMap<String, u64>,
    milestones: HashMap<String, u64>,
    pending: Vec<String>,
    aggregates: Vec<String>,
    retry: Option<Vec<String>>,
    invalid: BTreeMap<&'static str, u64>,
    operations_dirty: bool,
    summary_dirty: bool,
    spans: BTreeMap<u64, OpenSpan>,
    next_span: u64,
    recent_paints: VecDeque<((String, String), Instant)>,
}

impl Probe {
    fn new(record_prefix: String) -> Self {
        Self {
            record_prefix,
            next_record: 0,
            samples: BTreeMap::new(),
            counters: BTreeMap::new(),
            high_water: BTreeMap::new(),
            milestones: HashMap::new(),
            pending: Vec::new(),
            aggregates: Vec::new(),
            retry: None,
            invalid: BTreeMap::new(),
            operations_dirty: false,
            summary_dirty: false,
            spans: BTreeMap::new(),
            next_span: 0,
            recent_paints: VecDeque::new(),
        }
    }

    /// `fields` are stamped on the line only and never fork the summary.
    pub fn sample(&mut self, name: &str, ms: f64, fields: Option<Value>, t: i64) {
        if !ms.is_finite() || ms < 0. {
            return;
        }
        let bucket = self.samples.entry(name.to_owned()).or_default();
        bucket.push_back(ms);
        if bucket.len() > MAX_SAMPLES {
            bucket.pop_front();
        }
        self.summary_dirty = true;
        let mut line = Map::new();
        line.insert("t".into(), t.into());
        line.insert("name".into(), name.into());
        line.insert("ms".into(), json!((ms * 1000.).round() / 1000.));
        if let Some(Value::Object(fields)) = fields {
            line.extend(fields);
        }
        self.enqueue(Value::Object(line));
    }

    pub fn counter(&mut self, name: &str, delta: i64) {
        *self.counters.entry(name.to_owned()).or_default() += delta;
        self.operations_dirty = true;
    }

    pub fn high_water(&mut self, name: &str, value: u64) {
        if self
            .high_water
            .get(name)
            .is_some_and(|current| value <= *current)
        {
            return;
        }
        self.high_water.insert(name.to_owned(), value);
        self.operations_dirty = true;
    }

    pub fn milestone(&mut self, name: &str, at_ms: f64, t: i64) -> u64 {
        let occurrence = self.milestones.entry(name.to_owned()).or_default();
        *occurrence += 1;
        let occurrence = *occurrence;
        self.enqueue(json!({
            "t": t, "kind": "milestone", "name": name, "occurrence": occurrence, "atMs": at_ms,
        }));
        occurrence
    }

    /// One structured record. `kind` frames it: a field named `t` or `kind`
    /// cannot overwrite it, and a null field is left out.
    pub fn record(&mut self, kind: &str, fields: Value, t: i64) {
        let mut line = Map::new();
        line.insert("t".into(), t.into());
        line.insert("kind".into(), kind.into());
        if let Value::Object(fields) = fields {
            for (key, value) in fields {
                if key != "t" && key != "kind" && !value.is_null() {
                    line.insert(key, value);
                }
            }
        }
        self.enqueue(Value::Object(line));
    }

    fn identify(&mut self, mut record: Map<String, Value>) -> String {
        self.next_record += 1;
        record.insert(
            "recordId".into(),
            format!("{}:{}", self.record_prefix, self.next_record).into(),
        );
        Value::Object(record).to_string()
    }

    fn enqueue(&mut self, record: Value) {
        let Value::Object(record) = record else {
            return;
        };
        let line = self.identify(record);
        // serde_json escapes control characters, so a record is always one line.
        if line.len() > contract().max_line_bytes {
            self.invalid("renderer record exceeds UTF-8 byte limit");
        } else if self.pending.len() >= contract().max_lines_per_append * 4 {
            self.invalid("renderer pending line limit exceeded");
        } else {
            self.pending.push(line);
        }
    }

    fn invalid(&mut self, reason: &'static str) {
        *self.invalid.entry(reason).or_default() += 1;
    }

    pub fn has_evidence(&self) -> bool {
        self.retry.is_some()
            || !self.pending.is_empty()
            || !self.aggregates.is_empty()
            || !self.invalid.is_empty()
            || self.operations_dirty
            || self.summary_dirty
    }

    /// The next batch to append, or the one whose append failed, byte for byte.
    pub fn take_batch(&mut self, t: i64) -> Vec<String> {
        if let Some(retry) = &self.retry {
            return retry.clone();
        }
        let limit = contract().max_lines_per_append;
        let mut lines: Vec<String> = self
            .pending
            .drain(..self.pending.len().min(limit))
            .collect();
        while lines.len() < limit
            && let Some((reason, dropped)) = self.invalid.pop_first()
        {
            let record =
                json!({ "t": t, "kind": "sinkInvalid", "reason": reason, "droppedLines": dropped });
            if let Value::Object(record) = record {
                lines.push(self.identify(record));
            }
        }
        if lines.len() < limit && self.aggregates.is_empty() {
            self.prepare_aggregates(t);
        }
        let room = limit - lines.len();
        lines.extend(self.aggregates.drain(..self.aggregates.len().min(room)));
        self.retry = Some(lines.clone());
        lines
    }

    /// What became of the batch [`Probe::take_batch`] handed out. A failed one
    /// is kept apart from newer observations and retried unchanged.
    pub fn finish_batch(&mut self, appended: bool) {
        if appended {
            self.retry = None;
        }
    }

    fn prepare_aggregates(&mut self, t: i64) {
        if std::mem::take(&mut self.operations_dirty) {
            let rows: Vec<(bool, String, Value)> = self
                .counters
                .iter()
                .map(|(name, value)| (true, name.clone(), json!(value)))
                .chain(
                    self.high_water
                        .iter()
                        .map(|(name, value)| (false, name.clone(), json!(value))),
                )
                .collect();
            self.chunk(t, "operations", rows);
        }
        if std::mem::take(&mut self.summary_dirty) {
            let rows = self
                .summary()
                .into_iter()
                .map(|row| (true, String::new(), row))
                .collect();
            self.chunk(t, "summary", rows);
        }
    }

    /// Splits an aggregate into lines that each fit, dropping (and counting)
    /// only a row that cannot fit on a line of its own.
    fn chunk(&mut self, t: i64, kind: &'static str, rows: Vec<(bool, String, Value)>) {
        let budget = contract().max_line_bytes - RECORD_ID_ROOM;
        let render = |chunk: &[(bool, String, Value)]| -> Map<String, Value> {
            let mut record = Map::new();
            record.insert("t".into(), t.into());
            record.insert("kind".into(), kind.into());
            if kind == "summary" {
                let rows = chunk.iter().map(|(_, _, row)| row.clone()).collect();
                record.insert("summary".into(), Value::Array(rows));
            } else {
                let group = |counter: bool| {
                    let fields: Map<String, Value> = chunk
                        .iter()
                        .filter(|(is_counter, _, _)| *is_counter == counter)
                        .map(|(_, name, value)| (name.clone(), value.clone()))
                        .collect();
                    Value::Object(fields)
                };
                record.insert("counters".into(), group(true));
                record.insert("highWater".into(), group(false));
            }
            record
        };
        let mut chunk: Vec<(bool, String, Value)> = Vec::new();
        let mut records = Vec::new();
        for row in rows {
            chunk.push(row);
            if Value::Object(render(&chunk)).to_string().len() <= budget {
                continue;
            }
            let row = chunk.pop().expect("just pushed");
            if !chunk.is_empty() {
                records.push(render(&chunk));
            }
            chunk = vec![row];
        }
        if !chunk.is_empty() {
            records.push(render(&chunk));
        }
        for record in records {
            let line = self.identify(record);
            if line.len() <= contract().max_line_bytes {
                self.aggregates.push(line);
            } else {
                self.invalid("aggregate row exceeds UTF-8 byte limit");
            }
        }
    }

    /// Per-name percentiles over the retained samples.
    pub fn summary(&self) -> Vec<Value> {
        let round = |value: f64| (value * 1000.).round() / 1000.;
        self.samples
            .iter()
            .map(|(name, values)| {
                let mut sorted: Vec<f64> = values.iter().copied().collect();
                sorted.sort_by(f64::total_cmp);
                let at =
                    |fraction: f64| sorted[((sorted.len() - 1) as f64 * fraction).round() as usize];
                json!({
                    "name": name,
                    "n": sorted.len(),
                    "meanMs": round(sorted.iter().sum::<f64>() / sorted.len() as f64),
                    "p50Ms": round(at(0.5)),
                    "p95Ms": round(at(0.95)),
                    "maxMs": round(sorted[sorted.len() - 1]),
                })
            })
            .collect()
    }

    /// Opens an action's span; the ack names the pane it ends on.
    pub fn open_pane_span(&mut self, name: PaneSpan, scope: &str, now: Instant) -> u64 {
        self.prune_spans(now);
        while self.spans.len() >= MAX_PENDING_SPANS {
            self.spans.pop_first();
            self.counter("workflow.panePaintSpanCapacityDrops", 1);
        }
        self.next_span += 1;
        self.spans.insert(
            self.next_span,
            OpenSpan {
                name,
                scope: scope.to_owned(),
                started: now,
                pane_id: None,
            },
        );
        self.next_span
    }

    /// The ack named the pane. A pane that already painted since the action
    /// began finishes it at once: the paint can beat the ack.
    pub fn target_pane_span(&mut self, token: u64, pane_id: Option<&str>, t: i64) {
        let Some(pane_id) = pane_id else {
            self.spans.remove(&token);
            return;
        };
        let Some(span) = self.spans.get_mut(&token) else {
            return;
        };
        span.pane_id = Some(pane_id.to_owned());
        let key = (span.scope.clone(), pane_id.to_owned());
        let started = span.started;
        let painted = self
            .recent_paints
            .iter()
            .find(|(painted, _)| *painted == key)
            .map(|(_, at)| *at)
            .filter(|at| *at >= started);
        if let Some(painted_at) = painted {
            self.finish_span(token, painted_at, t);
        }
    }

    pub fn abandon_pane_span(&mut self, token: u64) {
        self.spans.remove(&token);
    }

    /// The client is gone: its spans can never end, and are not timeouts.
    pub fn abandon_pane_spans_for_scope(&mut self, scope: &str) {
        self.spans.retain(|_, span| span.scope != scope);
        self.recent_paints
            .retain(|((painted, _), _)| painted != scope);
    }

    /// A pane in `scope` painted its first content.
    pub fn note_pane_painted(&mut self, scope: &str, pane_id: &str, now: Instant, t: i64) {
        self.prune_spans(now);
        let key = (scope.to_owned(), pane_id.to_owned());
        self.recent_paints.retain(|(painted, _)| *painted != key);
        self.recent_paints.push_back((key, now));
        while self.recent_paints.len() > MAX_RECENT_PAINTS {
            self.recent_paints.pop_front();
        }
        let done: Vec<u64> = self
            .spans
            .iter()
            .filter(|(_, span)| span.scope == scope && span.pane_id.as_deref() == Some(pane_id))
            .map(|(token, _)| *token)
            .collect();
        for token in done {
            self.finish_span(token, now, t);
        }
    }

    fn finish_span(&mut self, token: u64, painted_at: Instant, t: i64) {
        if let Some(span) = self.spans.remove(&token) {
            let ms = painted_at
                .saturating_duration_since(span.started)
                .as_secs_f64()
                * 1000.;
            self.sample(span.name.name(), ms, None, t);
        }
    }

    fn prune_spans(&mut self, now: Instant) {
        let before = self.spans.len();
        self.spans
            .retain(|_, span| now.saturating_duration_since(span.started) < SPAN_TIMEOUT);
        let timed_out = before - self.spans.len();
        if timed_out > 0 {
            self.counter("workflow.panePaintSpanTimeouts", timed_out as i64);
        }
    }
}

// The process-wide probe.

struct Shared {
    probe: Probe,
    flush_scheduled: bool,
}

fn shared() -> Option<MutexGuard<'static, Shared>> {
    if !enabled() {
        return None;
    }
    static SHARED: OnceLock<Mutex<Shared>> = OnceLock::new();
    let shared = SHARED.get_or_init(|| {
        Mutex::new(Shared {
            probe: Probe::new(uuid::Uuid::new_v4().to_string()),
            flush_scheduled: false,
        })
    });
    shared.lock().ok()
}

fn wall_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

/// Whether this process opted in. A direct desktop-core query, as
/// `bootstrap.ts`'s `perf_log_enabled` invoke was.
pub fn enabled() -> bool {
    desktop_core::perf_log::sink::perf_log_enabled()
}

/// Runs `change` on the probe if the process opted in, then makes sure a
/// flush will follow.
fn with_probe(change: impl FnOnce(&mut Probe, i64)) {
    let Some(mut shared) = shared() else {
        return;
    };
    change(&mut shared.probe, wall_ms());
    if !shared.flush_scheduled && shared.probe.has_evidence() {
        shared.flush_scheduled = true;
        desktop_core::platform::spawn(flush_later());
    }
}

/// Appends whatever is buffered, batch by batch, on desktop-core's runtime. A
/// failed append is retried unchanged an interval later.
async fn flush_later() {
    loop {
        tokio::time::sleep(FLUSH_INTERVAL).await;
        if drain().await {
            return;
        }
    }
}

/// Appends until nothing is left (`true`) or an append fails (`false`). The
/// flush timer and the quit flush run one at a time: a batch in flight stays
/// the retry batch until its append settles, so a second drain would write it
/// again.
async fn drain() -> bool {
    static DRAINING: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    let _draining = DRAINING.lock().await;
    loop {
        let batch = {
            let Some(mut shared) = shared() else {
                return true;
            };
            if !shared.probe.has_evidence() {
                shared.flush_scheduled = false;
                return true;
            }
            shared.probe.take_batch(wall_ms())
        };
        let appended = desktop_core::perf_log::sink::append_perf_log(batch)
            .await
            .is_ok();
        let Some(mut shared) = shared() else {
            return true;
        };
        shared.probe.finish_batch(appended);
        if !appended {
            return false;
        }
    }
}

/// Writes what is buffered when the app quits, as `beforeunload` did: the
/// flush timer would otherwise lose the last two seconds.
pub fn init(cx: &mut gpui_kit::App) {
    if enabled() {
        cx.on_app_quit(|_| async {
            let _ = desktop_core::platform::spawn(drain()).await;
        })
        .detach();
    }
}

pub fn sample(name: &str, ms: f64) {
    with_probe(|probe, t| probe.sample(name, ms, None, t));
}

pub fn counter(name: &str, delta: i64) {
    with_probe(|probe, _| probe.counter(name, delta));
}

pub fn record(kind: &str, fields: Value) {
    with_probe(|probe, t| probe.record(kind, fields, t));
}

static ORIGIN: OnceLock<Instant> = OnceLock::new();

/// Pins the clock milestones count from, as `performance.now()` counts from
/// page start. Called first thing in `main`.
pub fn start_clock() {
    ORIGIN.get_or_init(Instant::now);
}

/// An ordered startup or interaction milestone, in milliseconds since start.
pub fn milestone(name: &str) {
    let at = ORIGIN.get_or_init(Instant::now).elapsed().as_secs_f64() * 1000.;
    with_probe(|probe, t| {
        probe.milestone(name, at, t);
    });
}

pub fn open_pane_span(name: PaneSpan, scope: &str) -> Option<u64> {
    let mut token = None;
    with_probe(|probe, _| token = Some(probe.open_pane_span(name, scope, Instant::now())));
    token
}

pub fn target_pane_span(token: Option<u64>, pane_id: Option<&str>) {
    if let Some(token) = token {
        with_probe(|probe, t| probe.target_pane_span(token, pane_id, t));
    }
}

pub fn abandon_pane_span(token: Option<u64>) {
    if let Some(token) = token {
        with_probe(|probe, _| probe.abandon_pane_span(token));
    }
}

pub fn abandon_pane_spans_for_scope(scope: &str) {
    with_probe(|probe, _| probe.abandon_pane_spans_for_scope(scope));
}

pub fn note_pane_painted(scope: &str, pane_id: &str) {
    with_probe(|probe, t| probe.note_pane_painted(scope, pane_id, Instant::now(), t));
}

/// One interaction's request-to-pixels measurement: it starts when made and
/// ends at the first paint the owner says is the one it was waiting for. Port
/// of `paintTicket.ts`; with the probe off there is no ticket at all.
pub struct PaintTicket {
    names: &'static [&'static str],
    started: Instant,
}

impl PaintTicket {
    pub fn start(names: &'static [&'static str]) -> Option<Self> {
        enabled().then(|| Self {
            names,
            started: Instant::now(),
        })
    }

    /// The content this ticket measured is on screen.
    pub fn painted(self) {
        let ms = self.started.elapsed().as_secs_f64() * 1000.;
        for name in self.names {
            sample(name, ms);
        }
    }
}

/// One surface's pending paint measurement and the rules for publishing it.
/// Port of `surfacePaint.ts`'s reporter: the loading side holds a ticket for
/// one load, keyed by its generation, and the view hands it out only once it
/// draws the load that was committed and is still current.
pub struct PaintReporter<T = PaintTicket> {
    pending: Option<(u64, T)>,
    committed: Option<u64>,
}

impl<T> Default for PaintReporter<T> {
    fn default() -> Self {
        Self {
            pending: None,
            committed: None,
        }
    }
}

impl<T> PaintReporter<T> {
    /// Takes over the measurement for the load at `generation`. With the
    /// probe off there is no ticket, and nothing changes.
    pub fn hold(&mut self, generation: u64, ticket: Option<T>) {
        if let Some(ticket) = ticket {
            self.pending = Some((generation, ticket));
        }
    }

    pub fn is_pending(&self) -> bool {
        self.pending.is_some()
    }

    /// Drops the measurement and forgets the committed load with it, so
    /// nothing held afterwards publishes before a fresh commit.
    pub fn abandon(&mut self) {
        self.pending = None;
        self.committed = None;
    }

    /// Retires one load's measurement: only if that load still holds the
    /// slot, never a successor's.
    pub fn discard(&mut self, generation: u64) {
        if self
            .pending
            .as_ref()
            .is_some_and(|(held, _)| *held == generation)
        {
            self.abandon();
        }
    }

    /// The load at `generation` is what the view now has to draw.
    pub fn note_committed(&mut self, generation: u64) {
        self.committed = Some(generation);
    }

    /// The view is drawing the load at `current`. The ticket comes out only
    /// when it belongs to that load and that load was committed; either way
    /// the slot is spent, as the surface's one chance to publish it.
    pub fn paintable(&mut self, current: u64) -> Option<T> {
        let (generation, ticket) = self.pending.take()?;
        (generation == current && self.committed == Some(generation)).then_some(ticket)
    }
}

/// Publishes `ticket` once the frame being drawn has reached the screen.
pub fn after_next_frame(ticket: Option<PaintTicket>, window: &gpui_kit::Window) {
    if let Some(ticket) = ticket {
        window.on_next_frame(move |_, _| ticket.painted());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn probe() -> Probe {
        Probe::new("run".into())
    }

    fn parse(lines: &[String]) -> Vec<Value> {
        lines
            .iter()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    fn drain(probe: &mut Probe) -> Vec<Value> {
        let mut lines = Vec::new();
        while probe.has_evidence() {
            let batch = probe.take_batch(1);
            assert!(batch.len() <= contract().max_lines_per_append);
            lines.extend(batch);
            probe.finish_batch(true);
        }
        parse(&lines)
    }

    #[test]
    fn coalesces_hot_path_counters_into_one_bounded_record() {
        let mut probe = probe();
        for index in 0..10_000 {
            probe.counter("terminal.events", 1);
            probe.high_water("terminal.queueDepth", index);
        }
        let batch = parse(&probe.take_batch(1));
        assert_eq!(batch.len(), 1);
        assert_eq!(batch[0]["kind"], "operations");
        assert_eq!(batch[0]["counters"]["terminal.events"], 10_000);
        assert_eq!(batch[0]["highWater"]["terminal.queueDepth"], 9_999);
    }

    #[test]
    fn retries_the_exact_batch_and_keeps_what_arrived_meanwhile_apart() {
        let mut probe = probe();
        probe.sample("terminal.event", 3., None, 1);
        let first = probe.take_batch(1);
        probe.finish_batch(false);
        probe.sample("arrived.meanwhile", 4., None, 1);
        assert_eq!(probe.take_batch(1), first, "a retry is byte for byte");
        probe.finish_batch(true);
        let rest = drain(&mut probe);
        assert!(rest.iter().any(|line| line["name"] == "arrived.meanwhile"));
    }

    #[test]
    fn chunks_past_the_native_line_limit_without_losing_samples() {
        let mut probe = probe();
        for index in 0..5_000 {
            probe.sample("burst", f64::from(index), None, 1);
        }
        let lines = drain(&mut probe);
        assert_eq!(
            lines.iter().filter(|line| line["name"] == "burst").count(),
            5_000
        );
    }

    #[test]
    fn byte_bounds_a_full_cardinality_aggregate() {
        let mut probe = probe();
        for index in 0..700 {
            probe.counter(
                &format!("fixture.counter.{index:04}.{}", "λ".repeat(12)),
                index,
            );
            probe.sample(
                &format!("fixture.sample.{index:04}"),
                index as f64 / 10.,
                None,
                1,
            );
        }
        let mut lines = Vec::new();
        while probe.has_evidence() {
            lines.extend(probe.take_batch(1));
            probe.finish_batch(true);
        }
        assert!(
            lines
                .iter()
                .all(|line| line.len() <= contract().max_line_bytes)
        );
        let records = parse(&lines);
        let kinds = |kind: &str| records.iter().filter(|line| line["kind"] == kind).count();
        assert!(kinds("operations") > 1);
        assert!(kinds("summary") > 1);
    }

    #[test]
    fn drops_one_oversized_aggregate_row_and_still_reports_later_ones() {
        let mut probe = probe();
        probe.counter(
            &format!("fixture.{}", "x".repeat(contract().max_line_bytes)),
            1,
        );
        let first = drain(&mut probe);
        probe.counter("fixture.afterOversize", 1);
        let later = drain(&mut probe);
        assert!(first.iter().any(|line| line["kind"] == "sinkInvalid"
            && line["reason"] == "aggregate row exceeds UTF-8 byte limit"
            && line["droppedLines"] == 1));
        assert!(
            later
                .iter()
                .any(|line| line["counters"]["fixture.afterOversize"] == 1)
        );
    }

    #[test]
    fn stamps_correlation_fields_on_the_sample_line_only() {
        let mut probe = probe();
        probe.sample(
            "file.open.segment.queueWait",
            12.5,
            Some(json!({ "operationId": "open-1" })),
            1,
        );
        let lines = drain(&mut probe);
        assert!(
            lines
                .iter()
                .any(|line| line["name"] == "file.open.segment.queueWait"
                    && line["ms"] == 12.5
                    && line["operationId"] == "open-1"
                    && line["recordId"] == "run:1")
        );
        let names: Vec<Value> = probe
            .summary()
            .iter()
            .map(|row| row["name"].clone())
            .collect();
        assert_eq!(names, [json!("file.open.segment.queueWait")]);
    }

    #[test]
    fn a_structured_record_is_one_line_its_fields_cannot_frame() {
        let mut probe = probe();
        probe.record(
            "perf.timeline",
            json!({
                "action": "selectSession",
                "kind": "must not overwrite the record's own label",
                "t": 7,
                "d1": 1_700_000_000_000_i64,
                "framesAheadByKind": { "terminalSeed": 4 },
                "absent": null,
                "text": "a\nb",
            }),
            5,
        );
        let lines = probe.take_batch(1);
        assert!(!lines[0].contains('\n'));
        let record: Value = serde_json::from_str(&lines[0]).unwrap();
        assert_eq!(record["kind"], "perf.timeline");
        assert_eq!(record["t"], 5);
        assert_eq!(record["framesAheadByKind"]["terminalSeed"], 4);
        assert!(record.get("absent").is_none());
    }

    #[test]
    fn closes_concurrent_spans_only_on_their_own_pane() {
        let mut probe = probe();
        let start = Instant::now();
        let names = |probe: &Probe| -> Vec<String> {
            probe
                .summary()
                .iter()
                .map(|row| row["name"].as_str().unwrap().to_owned())
                .collect()
        };
        let tab = probe.open_pane_span(PaneSpan::CreateTab, "client-a", start);
        let split = probe.open_pane_span(PaneSpan::PaneSplit, "client-a", start);
        probe.target_pane_span(tab, Some("%1"), 1);
        probe.target_pane_span(split, Some("%2"), 1);
        probe.note_pane_painted("client-a", "%2", start, 1);
        assert_eq!(names(&probe), ["pane.split"]);
        probe.note_pane_painted("client-b", "%1", start, 1);
        assert_eq!(names(&probe), ["pane.split"]);
        probe.note_pane_painted("client-a", "%1", start, 1);
        assert_eq!(names(&probe), ["create.tab", "pane.split"]);
        // The paint can come before the ack that names the pane.
        let late = probe.open_pane_span(PaneSpan::WindowSwitch, "client-a", start);
        probe.note_pane_painted("client-a", "%3", start + Duration::from_millis(5), 1);
        probe.target_pane_span(late, Some("%3"), 1);
        assert_eq!(names(&probe), ["create.tab", "pane.split", "window.switch"]);
        // A pane that painted before the action began does not end it.
        let early = probe.open_pane_span(
            PaneSpan::WindowSwitch,
            "client-a",
            start + Duration::from_secs(1),
        );
        probe.target_pane_span(early, Some("%1"), 1);
        assert_eq!(probe.spans.len(), 1);
    }

    #[test]
    fn a_gone_client_drops_its_spans_without_counting_timeouts() {
        let mut probe = probe();
        let start = Instant::now();
        let gone = probe.open_pane_span(PaneSpan::CreateTab, "client-a", start);
        probe.target_pane_span(gone, Some("%1"), 1);
        let kept = probe.open_pane_span(PaneSpan::CreateTab, "client-b", start);
        probe.abandon_pane_spans_for_scope("client-a");
        probe.note_pane_painted("client-a", "%1", start, 1);
        assert!(probe.summary().is_empty());
        assert_eq!(probe.spans.keys().copied().collect::<Vec<_>>(), [kept]);
        probe.prune_spans(start + SPAN_TIMEOUT);
        assert_eq!(probe.counters["workflow.panePaintSpanTimeouts"], 1);
    }

    #[test]
    fn bounds_spans_whose_pane_never_paints() {
        let mut probe = probe();
        let start = Instant::now();
        for index in 0..=MAX_PENDING_SPANS {
            probe.open_pane_span(PaneSpan::CreateTab, &format!("client-{index}"), start);
        }
        assert_eq!(probe.counters["workflow.panePaintSpanCapacityDrops"], 1);
        probe.open_pane_span(PaneSpan::CreateTab, "late", start + SPAN_TIMEOUT);
        assert_eq!(
            probe.counters["workflow.panePaintSpanTimeouts"],
            MAX_PENDING_SPANS as i64
        );
    }

    #[test]
    fn a_milestone_counts_its_occurrences() {
        let mut probe = probe();
        assert_eq!(probe.milestone("startup.terminalPaint", 12., 1), 1);
        assert_eq!(probe.milestone("startup.terminalPaint", 30., 1), 2);
        let lines = drain(&mut probe);
        assert_eq!(lines[1]["kind"], "milestone");
        assert_eq!(lines[1]["occurrence"], 2);
    }

    #[test]
    fn a_reporter_publishes_once_its_load_is_committed_and_drawn() {
        let mut reporter = PaintReporter::default();
        reporter.hold(3, Some("ticket"));
        reporter.note_committed(3);
        assert_eq!(reporter.paintable(3), Some("ticket"));
        assert_eq!(reporter.paintable(3), None, "published once");
    }

    #[test]
    fn a_superseded_load_retires_only_its_own_ticket() {
        let mut reporter = PaintReporter::default();
        reporter.hold(1, Some("first"));
        reporter.hold(2, Some("replacement"));
        reporter.discard(1);
        assert!(reporter.is_pending(), "the successor was dropped");
        reporter.note_committed(2);
        assert_eq!(reporter.paintable(2), Some("replacement"));
    }

    #[test]
    fn nothing_held_after_an_abandon_publishes_until_the_surface_commits_again() {
        let mut reporter = PaintReporter::default();
        reporter.hold(1, Some("first"));
        reporter.note_committed(1);
        reporter.abandon();
        reporter.hold(1, Some("next"));
        assert_eq!(reporter.paintable(1), None, "published against no commit");
        reporter.hold(1, Some("committed"));
        reporter.note_committed(1);
        assert_eq!(reporter.paintable(1), Some("committed"));
    }

    #[test]
    fn nothing_publishes_after_the_lifetime_that_armed_it_moved_on() {
        let mut reporter = PaintReporter::default();
        reporter.hold(1, Some("ticket"));
        reporter.note_committed(1);
        assert_eq!(reporter.paintable(2), None);
    }

    #[test]
    fn records_nothing_and_hands_out_no_ticket_while_the_probe_is_off() {
        // Tests never set `ADE_PERF_LOG`.
        assert!(!enabled());
        record("perf.timeline", json!({ "action": "x" }));
        // Port of `paintTicket.test.ts`: unmeasured traffic allocates no ticket.
        assert!(PaintTicket::start(&["workflow.file.editorPaint"]).is_none());
        assert!(open_pane_span(PaneSpan::CreateTab, "client").is_none());
    }
}
