//! Where the milliseconds of one host round trip actually went.
//!
//! The host can finish a workspace switch in six milliseconds and put its
//! answer on the socket with no queueing, and the desktop can still measure
//! seconds from issuing the action to holding the result. Only two places can
//! hold that time: the wire, behind whatever bytes the host had already sent
//! (a workspace switch makes it send every pane's screen), or the desktop
//! between reading the answer's bytes and resolving the caller's promise.
//!
//! This module stamps the second half of that timeline and counts the first.
//! Its numbers travel back to the renderer inside the invoke result, which
//! writes one `perf.timeline` record joining them to its own two stamps; see
//! `useTmuxActionPerformer.ts`.
//!
//! Everything here is compiled out of a plain release build with the rest of
//! `perf_log` — the inert twins in `stub.rs` keep every call site
//! unconditional — and, when compiled in, stays inert unless the process was
//! started with `ADE_PERF_LOG`.

use std::{
    io::Read,
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

use tmux_agent_protocol::v1::{self, envelope::Payload};

use super::sink::configured_path;

/// Both machines stamp wall clock, not a monotonic reading: the two halves of
/// the timeline are measured on different computers and only a shared epoch can
/// be joined. NTP skew is corrected in analysis, not here.
fn unix_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

/// One label per `v1::EventKind` discriminant, in discriminant order.
///
/// The histogram is what turns "the answer waited behind 1.8 MB" into "behind
/// 1.8 MB of *seeds*", which is the difference between a link problem and a
/// payload problem. A kind added to the protocol without a label here simply
/// lands in `other`; nothing breaks.
const EVENT_KIND_LABELS: [&str; 19] = [
    "unspecified",
    "topologySnapshot",
    "topologyDirty",
    "resyncRequired",
    "terminalSeed",
    "terminalOutput",
    "terminalExit",
    "terminalFlowPaused",
    "terminalResnapshotRequired",
    "paneResource",
    "terminalSeedDiagnostic",
    "activeRoot",
    "directorySnapshot",
    "fileChanged",
    "transferProgress",
    "gitStatus",
    "agentState",
    "terminalFlowStalled",
    "terminalClipboardWrite",
];

/// The frame shapes that are not host events, indexed after them.
const OTHER_KIND_LABELS: [&str; 3] = ["response", "fileStream", "other"];

const FRAME_KINDS: usize = EVENT_KIND_LABELS.len() + OTHER_KIND_LABELS.len();
const RESPONSE_KIND: usize = EVENT_KIND_LABELS.len();
const FILE_STREAM_KIND: usize = RESPONSE_KIND + 1;
const OTHER_KIND: usize = FILE_STREAM_KIND + 1;

fn kind_label(index: usize) -> &'static str {
    EVENT_KIND_LABELS
        .get(index)
        .copied()
        .unwrap_or_else(|| OTHER_KIND_LABELS[index - EVENT_KIND_LABELS.len()])
}

fn frame_kind(frame: &v1::Envelope) -> usize {
    match &frame.payload {
        Some(Payload::Response(_)) => RESPONSE_KIND,
        Some(Payload::FileStream(_)) => FILE_STREAM_KIND,
        Some(Payload::Event(event)) => usize::try_from(event.kind)
            .ok()
            .filter(|kind| *kind < EVENT_KIND_LABELS.len())
            .unwrap_or(OTHER_KIND),
        _ => OTHER_KIND,
    }
}

/// What the reader had consumed at one instant.
#[derive(Clone, Copy)]
pub(crate) struct LinkSnapshot {
    bytes: u64,
    frames: u64,
    kinds: [u64; FRAME_KINDS],
}

/// Everything one host link's reader has taken off the ssh stream.
///
/// Relaxed throughout: these are counters read by a different thread than the
/// one that writes them, and a count that is one frame stale changes no
/// conclusion the log supports. The alternative — ordering every frame read
/// against the request path — would make the instrument change what it
/// measures.
pub(crate) struct LinkCounters {
    bytes_read: AtomicU64,
    frames_read: AtomicU64,
    kinds: [AtomicU64; FRAME_KINDS],
}

impl LinkCounters {
    pub(crate) fn new() -> Self {
        Self {
            bytes_read: AtomicU64::new(0),
            frames_read: AtomicU64::new(0),
            kinds: [const { AtomicU64::new(0) }; FRAME_KINDS],
        }
    }

    pub(crate) fn bytes_read(&self) -> u64 {
        self.bytes_read.load(Ordering::Relaxed)
    }

    fn snapshot(&self, bytes: u64) -> LinkSnapshot {
        LinkSnapshot {
            bytes,
            frames: self.frames_read.load(Ordering::Relaxed),
            kinds: std::array::from_fn(|index| self.kinds[index].load(Ordering::Relaxed)),
        }
    }

    /// Counts one decoded frame and, for an answer, hands back what the reader
    /// had consumed *before* it — which is exactly the head-of-line evidence.
    ///
    /// `bytes_before` is read by the caller ahead of the frame, because the
    /// counting reader has already added this frame's own bytes by the time the
    /// envelope exists.
    pub(crate) fn note_frame_read(
        &self,
        frame: &v1::Envelope,
        bytes_before: u64,
    ) -> Option<AnswerMark> {
        let kind = frame_kind(frame);
        let mark = (kind == RESPONSE_KIND).then(|| AnswerMark {
            at_unix_millis: unix_millis(),
            snapshot: self.snapshot(bytes_before),
        });
        self.frames_read.fetch_add(1, Ordering::Relaxed);
        self.kinds[kind].fetch_add(1, Ordering::Relaxed);
        mark
    }

    /// Cumulative `(bytes, frames)` for `terminal_link_stats`, which is how the
    /// renderer's keystroke-echo probe gets the same head-of-line number for a
    /// measurement it owns entirely in TypeScript.
    pub(crate) fn totals(&self) -> Option<(u64, u64)> {
        configured_path()?;
        Some((
            self.bytes_read.load(Ordering::Relaxed),
            self.frames_read.load(Ordering::Relaxed),
        ))
    }
}

impl Default for LinkCounters {
    fn default() -> Self {
        Self::new()
    }
}

/// Counts what the frame parser consumes, above the `BufReader`.
///
/// Above rather than below on purpose: the question is how many bytes of other
/// frames the parser had to walk before it reached the answer, not how the
/// kernel happened to chunk them.
pub(crate) struct CountingReader<'a, R> {
    inner: R,
    counters: &'a LinkCounters,
}

impl<'a, R: Read> CountingReader<'a, R> {
    pub(crate) fn new(inner: R, counters: &'a LinkCounters) -> Self {
        Self { inner, counters }
    }
}

impl<R: Read> Read for CountingReader<'_, R> {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        let read = self.inner.read(buffer)?;
        self.counters
            .bytes_read
            .fetch_add(read as u64, Ordering::Relaxed);
        Ok(read)
    }
}

/// D4: the reader decoded an answer envelope, plus what it had read ahead of it.
///
/// Deliberately the decode instant rather than the answer's first byte: the
/// first byte would have to be stamped inside the shared frame codec, and an
/// answer frame is tens of bytes, so the two differ by less than the clock's
/// resolution. Every byte that could separate them is counted in `bytesAhead`.
#[derive(Clone, Copy)]
pub(crate) struct AnswerMark {
    at_unix_millis: u64,
    snapshot: LinkSnapshot,
}

struct Marks {
    /// The host's own id for this request, which is what joins this record to
    /// the daemon's `tmuxAction` and `responseWritten` lines.
    request_id: u64,
    /// D2: the invoke reached native.
    d2: u64,
    /// D3: the request frame's last byte was accepted by the ssh child's stdin.
    d3: Option<u64>,
    d3_snapshot: Option<LinkSnapshot>,
    in_flight_at_d3: Option<u64>,
    answer: Option<AnswerMark>,
}

/// The native half of one request's timeline, filled in as it happens.
///
/// Boxed behind an `Option` so an unmeasured process carries one null pointer
/// per request and touches no clock.
pub(crate) struct RequestTiming(Option<Box<Marks>>);

impl RequestTiming {
    /// Starts a measured request. Inert unless the process opted into the log.
    pub(crate) fn begin() -> Self {
        if configured_path().is_none() {
            return Self(None);
        }
        Self(Some(Box::new(Marks {
            request_id: 0,
            d2: unix_millis(),
            d3: None,
            d3_snapshot: None,
            in_flight_at_d3: None,
            answer: None,
        })))
    }

    pub(crate) fn inert() -> Self {
        Self(None)
    }

    /// D3, with the reader's counters and the delivery window's outstanding
    /// bytes at the same instant. `in_flight` is a closure because reading the
    /// window takes its lock, and an unmeasured build must not.
    pub(crate) fn mark_written(
        &mut self,
        request_id: u64,
        counters: &LinkCounters,
        in_flight: impl FnOnce() -> Option<u64>,
    ) {
        let Some(marks) = self.0.as_mut() else {
            return;
        };
        if marks.d3.is_some() {
            return;
        }
        marks.request_id = request_id;
        marks.d3 = Some(unix_millis());
        marks.d3_snapshot = Some(counters.snapshot(counters.bytes_read()));
        marks.in_flight_at_d3 = in_flight();
    }

    /// D4, as the reader stamped it on the answer frame.
    pub(crate) fn mark_answer(&mut self, answer: Option<AnswerMark>) {
        if let Some(marks) = self.0.as_mut() {
            marks.answer = answer;
        }
    }

    /// D5 and the record: the answer is about to be handed back to the invoke's
    /// caller. `None` when nothing was measured, which is every release build
    /// and every unmeasured launch.
    pub(crate) fn finish(self) -> Option<serde_json::Value> {
        let marks = self.0?;
        let d5 = unix_millis();
        let mut timing = serde_json::json!({
            "requestId": marks.request_id,
            "d2": marks.d2,
            "d3": marks.d3,
            "d5": d5,
        });
        if let Some(in_flight) = marks.in_flight_at_d3 {
            timing["inFlightAtD3"] = in_flight.into();
        }
        if let (Some(before), Some(answer)) = (marks.d3_snapshot, marks.answer) {
            let after = answer.snapshot;
            let by_kind = (0..FRAME_KINDS)
                .filter_map(|index| {
                    let ahead = after.kinds[index].saturating_sub(before.kinds[index]);
                    (ahead > 0).then(|| (kind_label(index).to_owned(), ahead.into()))
                })
                .collect::<serde_json::Map<String, serde_json::Value>>();
            timing["d4"] = answer.at_unix_millis.into();
            timing["bytesAhead"] = after.bytes.saturating_sub(before.bytes).into();
            timing["framesAhead"] = after.frames.saturating_sub(before.frames).into();
            timing["framesAheadByKind"] = by_kind.into();
        }
        Some(timing)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tmux_agent_protocol::envelope;

    fn event(kind: v1::EventKind) -> v1::Envelope {
        envelope(
            0,
            1,
            Payload::Event(v1::HostEvent {
                kind: kind.into(),
                ..Default::default()
            }),
        )
    }

    #[test]
    fn every_frame_kind_has_a_label_and_events_index_by_discriminant() {
        for index in 0..FRAME_KINDS {
            assert!(!kind_label(index).is_empty());
        }
        assert_eq!(
            frame_kind(&event(v1::EventKind::TerminalSeed)),
            v1::EventKind::TerminalSeed as usize
        );
        assert_eq!(
            kind_label(frame_kind(&event(v1::EventKind::TerminalOutput))),
            "terminalOutput"
        );
        assert_eq!(
            kind_label(frame_kind(&envelope(
                7,
                0,
                Payload::Response(v1::Response::default())
            ))),
            "response"
        );
    }

    #[test]
    fn an_answer_is_marked_with_what_the_reader_read_ahead_of_it() {
        let counters = LinkCounters::new();
        let mut bytes = 0;
        for _ in 0..3 {
            let before = counters.bytes_read();
            bytes += 100;
            counters
                .bytes_read
                .store(bytes, std::sync::atomic::Ordering::Relaxed);
            assert!(
                counters
                    .note_frame_read(&event(v1::EventKind::TerminalSeed), before)
                    .is_none()
            );
        }
        let before = counters.bytes_read();
        counters
            .bytes_read
            .store(bytes + 40, std::sync::atomic::Ordering::Relaxed);
        let mark = counters
            .note_frame_read(
                &envelope(9, 0, Payload::Response(v1::Response::default())),
                before,
            )
            .expect("a response frame must carry a mark");
        // The answer's own 40 bytes are not "ahead" of it.
        assert_eq!(mark.snapshot.bytes, 300);
        assert_eq!(mark.snapshot.frames, 3);
        assert_eq!(mark.snapshot.kinds[v1::EventKind::TerminalSeed as usize], 3);
    }

    #[test]
    fn an_unmeasured_process_records_nothing_and_reads_no_clock() {
        // `ADE_PERF_LOG` is unset under test, so `begin` must stay inert and
        // `finish` must produce no record for the invoke result to carry.
        let mut timing = RequestTiming::begin();
        timing.mark_written(1, &LinkCounters::new(), || panic!("must not be consulted"));
        assert!(timing.finish().is_none());
    }
}
