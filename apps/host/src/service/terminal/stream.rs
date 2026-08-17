use std::{
    collections::HashMap,
    io::{BufReader, Read},
    process::ChildStdout,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc as std_mpsc,
    },
};

use tmux_agent_protocol::v1;
use tmux_control::{
    CommandTag, ControlParser, ControlRecord, OutputDisposition, PaneResourceStore, ScreenSeeder,
};
use tokio::sync::mpsc;

use super::super::{SequencerControl, emit_event};
use super::OutputCredit;
use super::correlation::{
    MarkerBlock, classify_marker_block, error_reason, marker_pane, wants_error_line,
};
use super::flow_control::RejectedResume;
use super::{build_seed_with_metadata, capture_metadata, validate_tmux_id};

/// Asks the control-writer thread to write a capture for `pane_id`.
///
/// The send cannot block and cannot drop: the lane is unbounded and exists
/// solely for these writes. That matters most for `resume_first`, which carries
/// the only `refresh-client -A <pane>:continue` in the host — losing one would
/// leave tmux's flow control holding that pane's output forever.
fn request_capture(
    writer: &std_mpsc::Sender<super::ControlWrite>,
    pane_id: &str,
    resume_first: bool,
) {
    send_capture(writer, pane_id, resume_first, None);
}

/// How long a retried resume waits, so the second attempt is a second chance
/// rather than the same attempt written twice. Short enough that a pane the
/// user is watching is not visibly held, long enough for whatever was in
/// flight when the first was refused to have finished.
const RESUME_RETRY_DELAY: std::time::Duration = std::time::Duration::from_millis(150);

fn send_capture(
    writer: &std_mpsc::Sender<super::ControlWrite>,
    pane_id: &str,
    resume_first: bool,
    delay: Option<std::time::Duration>,
) {
    let _ = writer.send(super::ControlWrite {
        pane_id: pane_id.to_owned(),
        resume_first,
        delay,
    });
}

pub(super) struct ControlStreamReader {
    pub(super) stdout: ChildStdout,
    /// Writes the reader needs performed are handed to the input dispatch
    /// thread. The reader itself must never write to tmux's stdin: tmux stops
    /// reading stdin while it is blocked writing output, and the reader is the
    /// only thing draining that output, so a write from here can deadlock the
    /// pair and silence the pane permanently.
    pub(super) writer: std_mpsc::Sender<super::ControlWrite>,
    pub(super) pane_ids: Vec<String>,
    pub(super) event_tx: mpsc::Sender<SequencerControl>,
    pub(super) overflowed: Arc<AtomicBool>,
    pub(super) resources: Arc<Mutex<PaneResourceStore>>,
    pub(super) terminal_generation: Arc<AtomicU64>,
    pub(super) stopped: Arc<AtomicBool>,
    pub(super) controls: std_mpsc::Receiver<StreamControl>,
    pub(super) flow: Arc<super::FlowControl>,
    pub(super) output_credit: Arc<OutputCredit>,
    pub(super) emission_order: Arc<Mutex<()>>,
}

pub(super) enum StreamControl {
    Membership {
        /// Authoritative replacement membership, rather than a relative
        /// delta. Delivery precedes the fallible tmux stdin batch, so a retry
        /// must converge idempotently even when the desired set changed.
        pane_ids: Vec<String>,
    },
}

pub(super) fn read_control_stream(context: ControlStreamReader) {
    let ControlStreamReader {
        stdout,
        writer,
        pane_ids,
        event_tx,
        overflowed,
        resources,
        terminal_generation,
        stopped,
        controls,
        flow,
        output_credit,
        emission_order,
    } = context;
    let mut reader = BufReader::new(stdout);
    let mut parser = ControlParser::default();
    let mut state = StreamState::new(&pane_ids, flow);
    let mut buffer = [0_u8; 64 * 1024];
    let mut pending_output = PendingOutput::default();
    let runtime = || StreamRuntime {
        writer: &writer,
        sender: &event_tx,
        overflowed: &overflowed,
        resources: &resources,
        terminal_generation: &terminal_generation,
        stopped: &stopped,
        output_credit: &output_credit,
        emission_order: &emission_order,
    };
    loop {
        match reader.read(&mut buffer) {
            Ok(0) => break,
            Ok(length) => {
                while let Ok(control) = controls.try_recv() {
                    state.apply_control(control);
                }
                parser.push(&buffer[..length]);
                while let Some(record) = parser.next_record() {
                    match record {
                        Ok(ControlRecord::Output { pane_id, data }) => {
                            for output in pending_output.push(pane_id, data) {
                                state.handle(output, runtime());
                            }
                        }
                        Ok(record) => {
                            if let Some(output) = pending_output.take() {
                                state.handle(output, runtime());
                            }
                            state.handle(record, runtime());
                        }
                        Err(error) => {
                            if let Some(output) = pending_output.take() {
                                state.handle(output, runtime());
                            }
                            emit_resnapshot(&event_tx, &overflowed, "terminal", error.to_string());
                            state.resnapshot_all(&writer);
                        }
                    }
                }
                // Bound first-byte latency without giving each `%output` line
                // its own protobuf record. One read is at most 64 KiB, so a
                // sustained pane flood becomes roughly one sequencer entry per
                // read while a lone keystroke echo is still published now.
                if let Some(output) = pending_output.take() {
                    state.handle(output, runtime());
                }
            }
            Err(_) => break,
        }
    }
    parser.finish();
    while let Some(Err(error)) = parser.next_record() {
        emit_resnapshot(&event_tx, &overflowed, "terminal", error.to_string());
    }
    if !stopped.load(Ordering::Acquire) {
        state.emit_pane_scoped_recovery(
            &event_tx,
            &overflowed,
            "tmux session control stream ended",
        );
    }
}

const MAX_COALESCED_OUTPUT_BYTES: usize = 64 * 1024;

#[derive(Default)]
struct PendingOutput {
    pane_id: Option<String>,
    data: Vec<u8>,
}

impl PendingOutput {
    /// Adds one parsed output record and returns the previous batch when pane
    /// identity or the byte bound requires an ordering boundary.
    fn push(&mut self, pane_id: String, data: Vec<u8>) -> Vec<ControlRecord> {
        let mut ready = Vec::new();
        if self
            .pane_id
            .as_deref()
            .is_some_and(|current| current != pane_id)
            && let Some(output) = self.take()
        {
            ready.push(output);
        }
        for chunk in data.chunks(MAX_COALESCED_OUTPUT_BYTES) {
            if self.pane_id.is_none() {
                self.pane_id = Some(pane_id.clone());
            }
            let remaining = MAX_COALESCED_OUTPUT_BYTES - self.data.len();
            let (head, tail) = chunk.split_at(remaining.min(chunk.len()));
            self.data.extend_from_slice(head);
            if self.data.len() == MAX_COALESCED_OUTPUT_BYTES
                && (!tail.is_empty() || chunk.as_ptr_range().end != data.as_ptr_range().end)
                && let Some(output) = self.take()
            {
                ready.push(output);
                self.pane_id = Some(pane_id.clone());
            }
            self.data.extend_from_slice(tail);
        }
        ready
    }

    fn take(&mut self) -> Option<ControlRecord> {
        let pane_id = self.pane_id.take()?;
        Some(ControlRecord::Output {
            pane_id,
            data: std::mem::take(&mut self.data),
        })
    }
}

#[derive(Debug)]
pub(super) enum PaneSeedState {
    Pending {
        buffered: Vec<(u64, Vec<u8>)>,
        buffered_bytes: usize,
        overflowed: bool,
    },
    Live,
}

#[derive(Debug)]
pub(super) enum CommandBlock {
    None,
    Unknown {
        tag: CommandTag,
        pane_id: Option<String>,
        lines: Vec<Vec<u8>>,
    },
    /// One `refresh-client -A '%N:continue'`, correlated to the pane its marker
    /// named. A rejected resume leaves that pane paused forever, so it must be
    /// reported against the pane rather than the connection.
    Resume {
        tag: CommandTag,
        pane_id: String,
        lines: Vec<Vec<u8>>,
    },
    CapturePrimary {
        tag: CommandTag,
        pane_id: String,
        lines: Vec<Vec<u8>>,
    },
    CaptureAlternate {
        tag: CommandTag,
        pane_id: String,
        visible_lines: Vec<Vec<u8>>,
        visible_boundary: u64,
        lines: Vec<Vec<u8>>,
    },
    CaptureMetadata {
        tag: CommandTag,
        pane_id: String,
        visible_lines: Vec<Vec<u8>>,
        saved_normal_lines: Vec<Vec<u8>>,
        visible_boundary: u64,
        lines: Vec<Vec<u8>>,
    },
}

pub(super) struct StreamState {
    pub(super) pane_states: HashMap<String, PaneSeedState>,
    pub(super) expected_capture: Option<String>,
    pub(super) expected_resume: Option<String>,
    pub(super) pending_alternate: Option<(String, Vec<Vec<u8>>, u64)>,
    pub(super) pending_metadata: Option<PendingCaptureMetadata>,
    command_block: CommandBlock,
    /// Shared with the service thread, which is what writes a seed when the
    /// desktop reveals a pane: a seed for a pane tmux has paused has to carry
    /// the resume or it re-photographs a screen that then stops moving again.
    flow: Arc<super::FlowControl>,
}

pub(super) struct PendingCaptureMetadata {
    pub(super) pane_id: String,
    pub(super) visible_lines: Vec<Vec<u8>>,
    pub(super) saved_normal_lines: Vec<Vec<u8>>,
    pub(super) visible_boundary: u64,
}

struct StreamRuntime<'a> {
    writer: &'a std_mpsc::Sender<super::ControlWrite>,
    sender: &'a mpsc::Sender<SequencerControl>,
    overflowed: &'a AtomicBool,
    resources: &'a Arc<Mutex<PaneResourceStore>>,
    terminal_generation: &'a Arc<AtomicU64>,
    stopped: &'a AtomicBool,
    output_credit: &'a OutputCredit,
    emission_order: &'a Arc<Mutex<()>>,
}

impl StreamState {
    pub(super) fn new(pane_ids: &[String], flow: Arc<super::FlowControl>) -> Self {
        Self {
            flow,
            pane_states: pane_ids
                .iter()
                .map(|id| {
                    (
                        id.clone(),
                        PaneSeedState::Pending {
                            buffered: Vec::new(),
                            buffered_bytes: 0,
                            overflowed: false,
                        },
                    )
                })
                .collect(),
            expected_capture: None,
            expected_resume: None,
            pending_alternate: None,
            pending_metadata: None,
            command_block: CommandBlock::None,
        }
    }

    fn handle(&mut self, record: ControlRecord, runtime: StreamRuntime<'_>) {
        let StreamRuntime {
            writer,
            sender,
            overflowed,
            resources,
            terminal_generation,
            stopped,
            output_credit,
            emission_order,
        } = runtime;
        if stopped.load(Ordering::Acquire) {
            return;
        }
        match record {
            ControlRecord::Output { pane_id, data } => match self.pane_states.get_mut(&pane_id) {
                Some(PaneSeedState::Pending {
                    buffered,
                    buffered_bytes,
                    overflowed: replay_overflowed,
                }) => {
                    let output_generation = terminal_generation.fetch_add(1, Ordering::AcqRel) + 1;
                    if buffered_bytes.saturating_add(data.len()) > 4 * 1024 * 1024 {
                        buffered.clear();
                        *buffered_bytes = 0;
                        *replay_overflowed = true;
                        emit_resnapshot(
                            sender,
                            overflowed,
                            &pane_id,
                            format!("screen replay buffer overflow for {pane_id}"),
                        );
                    } else {
                        *buffered_bytes += data.len();
                        buffered.push((output_generation, data));
                    }
                }
                _ => OutputEmission {
                    sender,
                    overflowed,
                    resources,
                    terminal_generation,
                    stopped,
                    output_credit,
                    emission_order,
                }
                .record(pane_id, data),
            },
            ControlRecord::Begin { tag, .. } => {
                if !matches!(self.command_block, CommandBlock::None) {
                    let scope = self.active_scope();
                    emit_resnapshot(
                        sender,
                        overflowed,
                        &scope,
                        format!(
                            "command {} began before the prior command ended",
                            tag.number
                        ),
                    );
                }
                self.command_block = self.start_block(tag);
            }
            ControlRecord::CommandOutput(line) => match &mut self.command_block {
                CommandBlock::Unknown { pane_id, lines, .. } => {
                    if pane_id.is_none() {
                        *pane_id = marker_pane(&line);
                    }
                    lines.push(line);
                }
                // `send-keys` and `refresh-client` print nothing when they
                // succeed; a line here is the rejection reason, and carrying a
                // few of them is what makes the `%error` below readable. The
                // bound keeps a misrouted block from growing a log detail
                // without limit.
                CommandBlock::Resume { lines, .. } => {
                    if wants_error_line(lines.len()) {
                        lines.push(line);
                    }
                }
                CommandBlock::CapturePrimary { lines, .. }
                | CommandBlock::CaptureAlternate { lines, .. }
                | CommandBlock::CaptureMetadata { lines, .. } => lines.push(line),
                CommandBlock::None => emit_resnapshot(
                    sender,
                    overflowed,
                    "terminal",
                    "command output arrived without a begin record".into(),
                ),
            },
            ControlRecord::End { tag, .. } => self.finish_block(
                tag,
                StreamRuntime {
                    writer,
                    sender,
                    overflowed,
                    resources,
                    terminal_generation,
                    stopped,
                    output_credit,
                    emission_order,
                },
            ),
            ControlRecord::Error { tag, arguments } => {
                if !self.active_tag_matches(tag) {
                    let scope = self.active_scope();
                    emit_resnapshot(
                        sender,
                        overflowed,
                        &scope,
                        format!("mismatched error command tag {}", tag.number),
                    );
                }
                let scope = self.active_scope();
                // Input acks are fire-and-forget on the desktop side, so a
                // rejected keystroke reaches the user only here. Scoping the
                // recovery to the pane keeps one dead pane from resnapshotting
                // the whole connection, and the detail names the cause.
                //
                // `arguments` is only the `%error` header's three numbers. What
                // tmux actually objected to arrived as the block's output lines
                // ("parse error: syntax error"), so a detail without them names
                // no cause at all — that is why P12-U001 was invisible in every
                // log it produced.
                //
                // Only the blocks that print nothing on success contribute
                // their lines. A capture block's lines are the user's screen;
                // quoting four rows of it into an event that reaches the
                // desktop and the logs would leak the pane, not explain the
                // failure.
                let (detail, rejected_resume) = match &self.command_block {
                    CommandBlock::Resume { pane_id, lines, .. } => (
                        format!(
                            "tmux rejected the flow-control resume for {pane_id}: {}",
                            error_reason(&arguments, lines)
                        ),
                        Some(pane_id.clone()),
                    ),
                    _ => (arguments, None),
                };
                // An error abandons whatever multi-block sequence was running,
                // so every correlation slot has to be released too — otherwise
                // the next unrelated block is mistaken for the missing half of
                // this one.
                self.command_block = CommandBlock::None;
                self.expected_capture = None;
                self.expected_resume = None;
                self.pending_alternate = None;
                self.pending_metadata = None;
                // Exactly one event per rejection, and for a rejected resume
                // which one it is depends on what this thread is about to do.
                //
                // Both a resnapshot and a stall make the desktop ask for a
                // seed, so emitting both would ask twice — and each of those
                // seeds carries a resume for a pane still believed paused,
                // which the same broken command rejects, which emits both
                // events again. Two seeds per rejection is 2ⁿ tmux writes.
                // While the host is still retrying, the desktop has nothing to
                // do and is told nothing; when the host gives up, the stall is
                // both the report and the request for the seed that replaces
                // it.
                let decision =
                    rejected_resume.map(|pane_id| (self.flow.reject_resume(&pane_id), pane_id));
                if let Some((disposition, pane_id)) = &decision {
                    crate::diagnostics::write_flow_resume_rejected_log(
                        pane_id,
                        disposition.label(),
                        &detail,
                    );
                }
                match decision {
                    // Re-issued rather than abandoned: the pane is still paused,
                    // and a resume is the only thing that changes that. Unlike a
                    // bare capture re-issued against a vanished pane, this one
                    // carries its own targeted marker, so a second failure is
                    // attributed here again instead of to the connection.
                    Some((RejectedResume::Retry, pane_id)) => {
                        send_capture(writer, &pane_id, true, Some(RESUME_RETRY_DELAY))
                    }
                    // Out of attempts. `reject_resume` has already stopped this
                    // pane's captures carrying a resume, so the seed asked for
                    // here is a plain capture and the recovery terminates.
                    Some((RejectedResume::Stall, pane_id)) => {
                        emit_event(
                            sender,
                            overflowed,
                            v1::HostEvent {
                                kind: v1::EventKind::TerminalFlowStalled.into(),
                                scope: pane_id,
                                detail,
                                ..Default::default()
                            },
                        );
                    }
                    // Every other rejection, including a resume for a pane
                    // nothing paused, is a command the desktop has to be told
                    // about and has always been told about this way.
                    Some((RejectedResume::Ignore, _)) | None => {
                        emit_resnapshot(sender, overflowed, &scope, detail)
                    }
                }
            }
            ControlRecord::Exit { reason } => {
                if !stopped.load(Ordering::Acquire) {
                    // tmux sends `%exit` whenever this control client's session
                    // ends, which includes the ordinary case of the user
                    // closing a workspace. Scoping the recovery to this
                    // client's panes keeps that from asking the desktop for a
                    // connection-wide resnapshot — a full bridge reconnect and
                    // a reseed of every pane in every other workspace.
                    let detail = if reason.is_empty() {
                        "tmux session control client exited".to_owned()
                    } else {
                        reason
                    };
                    self.emit_pane_scoped_recovery(sender, overflowed, &detail);
                    // This client *was* the notification source for its session,
                    // so its death produces no topology change to notice. Say so
                    // explicitly: reconciliation is what replaces the client, and
                    // without this the panes stay dead until the safety pass.
                    emit_event(
                        sender,
                        overflowed,
                        v1::HostEvent {
                            kind: v1::EventKind::TopologyDirty.into(),
                            scope: "topology".into(),
                            detail: "session control client exited".into(),
                            ..Default::default()
                        },
                    );
                    stopped.store(true, Ordering::Release);
                }
            }
            ControlRecord::Notification { name, arguments } if name == "pause" => {
                // Only for a pane this client actually carries. The control
                // client is attached to the whole session, but membership is
                // the mounted subset, and a capture for a pane outside it is
                // correlated to nothing: `start_block` filters the marker away
                // (see `expected_capture`), so the capture-pane reply lands in
                // an `Unknown` block that accumulates the pane's screen and is
                // then scanned for marker prefixes. An unmounted pane's output
                // is not delivered anyway, so there is nothing to resume it
                // for.
                if let Some(pane_id) =
                    notification_pane(&arguments).filter(|id| self.pane_states.contains_key(id))
                {
                    self.flow.paused(&pane_id);
                    emit_event(
                        sender,
                        overflowed,
                        v1::HostEvent {
                            kind: v1::EventKind::TerminalFlowPaused.into(),
                            scope: pane_id.clone(),
                            detail: "tmux pause-after flow control engaged".into(),
                            ..Default::default()
                        },
                    );
                    request_capture(writer, &pane_id, true);
                }
            }
            // tmux says `%continue` only for a pane that really was paused, so
            // this is the acknowledgement the resume itself is not. Until it
            // arrives every capture for the pane carries a resume, which is the
            // safe direction to be wrong in — see `FlowControl`.
            ControlRecord::Notification { name, arguments } if name == "continue" => {
                if let Some(pane_id) = notification_pane(&arguments) {
                    self.flow.cleared(&pane_id);
                }
            }
            ControlRecord::Notification { name, .. } if is_topology_notification(&name) => {
                emit_event(
                    sender,
                    overflowed,
                    v1::HostEvent {
                        kind: v1::EventKind::TopologyDirty.into(),
                        scope: "topology".into(),
                        detail: name,
                        ..Default::default()
                    },
                );
            }
            _ => {}
        }
    }

    fn finish_block(&mut self, end_tag: CommandTag, runtime: StreamRuntime<'_>) {
        let StreamRuntime {
            writer,
            sender,
            overflowed,
            resources,
            terminal_generation,
            stopped,
            output_credit,
            emission_order,
        } = runtime;
        if stopped.load(Ordering::Acquire) {
            self.command_block = CommandBlock::None;
            return;
        }
        if !self.active_tag_matches(end_tag) {
            let scope = self.active_scope();
            emit_resnapshot(
                sender,
                overflowed,
                &scope,
                format!("mismatched end command tag {}", end_tag.number),
            );
            self.command_block = CommandBlock::None;
            return;
        }
        match std::mem::replace(&mut self.command_block, CommandBlock::None) {
            CommandBlock::Unknown { pane_id, lines, .. } => {
                match classify_marker_block(pane_id, &lines) {
                    MarkerBlock::Input { .. } => emit_resnapshot(
                        sender,
                        overflowed,
                        "terminal",
                        "input marker arrived on an output-only tmux client".into(),
                    ),
                    MarkerBlock::Resume(pane_id) => self.expected_resume = Some(pane_id),
                    MarkerBlock::Capture(pane_id) => {
                        self.expected_capture =
                            pane_id.filter(|pane_id| self.pane_states.contains_key(pane_id));
                    }
                }
            }
            // A resume that ends without an error is not an ack: tmux emits
            // `%continue` only when a pane really was paused, and says nothing
            // at all otherwise. The capture written with it is what actually
            // recovers the pane, because output produced while paused is
            // dropped rather than replayed.
            CommandBlock::Resume { .. } => {}
            CommandBlock::CapturePrimary { pane_id, lines, .. } => {
                // tmux emits one %begin/%end block per command separated by
                // `;`: capture-pane and its following display-message metadata
                // are distinct correlated blocks.
                let visible_boundary = terminal_generation.load(Ordering::Acquire);
                self.pending_alternate = Some((pane_id, lines, visible_boundary));
            }
            CommandBlock::CaptureAlternate {
                pane_id,
                visible_lines,
                visible_boundary,
                lines,
                ..
            } => {
                // This is the precise output boundary represented by both
                // screen captures. Later terminal output is replayed once.
                self.pending_metadata = Some(PendingCaptureMetadata {
                    pane_id,
                    visible_lines,
                    saved_normal_lines: lines,
                    visible_boundary,
                });
            }
            CommandBlock::CaptureMetadata {
                pane_id,
                visible_lines,
                saved_normal_lines,
                visible_boundary,
                lines,
                ..
            } => {
                let mut retry = false;
                if let Some(state) = self.pane_states.get_mut(&pane_id) {
                    if let PaneSeedState::Pending {
                        buffered,
                        overflowed: replay_overflowed,
                        ..
                    } = state
                    {
                        retry = *replay_overflowed;
                        if !retry {
                            if let Some(metadata) = capture_metadata(&lines, &pane_id) {
                                // The screen the user sees always comes from
                                // the first capture — plain `capture-pane`
                                // returns the displayed grid in both screen
                                // modes — so that is the point this seed is
                                // current through. Output that landed between
                                // the two captures is replayed after it.
                                let capture_boundary = visible_boundary;
                                let seed_build = build_seed_with_metadata(
                                    visible_lines,
                                    saved_normal_lines,
                                    metadata,
                                );
                                let mut seeder = ScreenSeeder::default();
                                for (generation, data) in buffered.drain(..) {
                                    if !seeder.buffer(generation, data) {
                                        retry = true;
                                        break;
                                    }
                                }
                                if retry {
                                    emit_resnapshot(
                                        sender,
                                        overflowed,
                                        &pane_id,
                                        format!("screen replay buffer overflow for {pane_id}"),
                                    );
                                } else {
                                    let replay =
                                        seeder.complete(seed_build.bytes, capture_boundary);
                                    let _emission = emission_order.lock().unwrap();
                                    let seed_generation =
                                        terminal_generation.fetch_add(1, Ordering::AcqRel) + 1;
                                    let seed = replay.seed;
                                    let replay_outputs = replay.replay;
                                    let diagnostics = seed_build.diagnostics;
                                    let visible =
                                        with_active_resources(resources, stopped, |resources| {
                                            resources.snapshot(
                                                &pane_id,
                                                seed.clone(),
                                                seed_generation,
                                            );
                                            !resources.is_hidden(&pane_id)
                                        })
                                        .unwrap_or(false);
                                    if visible {
                                        if !diagnostics.is_empty() {
                                            emit_event(
                                                sender,
                                                overflowed,
                                                v1::HostEvent {
                                                    kind: v1::EventKind::TerminalSeedDiagnostic
                                                        .into(),
                                                    scope: pane_id.clone(),
                                                    detail: diagnostics.join("; "),
                                                    ..Default::default()
                                                },
                                            );
                                        }
                                        emit_terminal(
                                            sender,
                                            overflowed,
                                            v1::EventKind::TerminalSeed,
                                            pane_id.clone(),
                                            seed,
                                            seed_generation,
                                            output_credit,
                                        );
                                    }
                                    drop(_emission);
                                    for output in replay_outputs {
                                        let _emission = emission_order.lock().unwrap();
                                        // Buffered sequence numbers establish
                                        // capture inclusion only. Rebase
                                        // replay delivery after the seed so a
                                        // renderer's monotonic generation gate
                                        // cannot discard required output.
                                        let replay_generation =
                                            terminal_generation.fetch_add(1, Ordering::AcqRel) + 1;
                                        let visible = with_active_resources(
                                            resources,
                                            stopped,
                                            |resources| {
                                                resources.record_output(
                                                    &pane_id,
                                                    &output.bytes,
                                                    replay_generation,
                                                ) == OutputDisposition::Visible
                                            },
                                        )
                                        .unwrap_or(false);
                                        if visible {
                                            emit_terminal(
                                                sender,
                                                overflowed,
                                                v1::EventKind::TerminalOutput,
                                                pane_id.clone(),
                                                output.bytes,
                                                replay_generation,
                                                output_credit,
                                            );
                                        }
                                    }
                                }
                            } else {
                                emit_resnapshot(
                                    sender,
                                    overflowed,
                                    &pane_id,
                                    format!("capture metadata parity failed for {pane_id}"),
                                );
                                retry = true;
                            }
                        }
                    }
                    if !retry {
                        *state = PaneSeedState::Live;
                    }
                }
                if retry {
                    emit_resnapshot(
                        sender,
                        overflowed,
                        &pane_id,
                        format!("discarded incomplete seed for {pane_id}"),
                    );
                    // The pane is still Pending, so its output is being
                    // buffered rather than delivered: leaving it that way waits
                    // for the client to ask again, and a client that does not
                    // will never hear from this pane again. Ask tmux for
                    // another capture here. The replay buffer is the rate
                    // limiter — a discard needs another few megabytes of output
                    // before it can happen again — so even a permanent flood
                    // costs a couple of in-band commands per megabyte.
                    request_capture(writer, &pane_id, self.flow.resume_before_capture(&pane_id));
                }
            }
            CommandBlock::None => {}
        }
    }

    fn active_tag_matches(&self, tag: CommandTag) -> bool {
        match &self.command_block {
            CommandBlock::Unknown { tag: active, .. }
            | CommandBlock::Resume { tag: active, .. }
            | CommandBlock::CapturePrimary { tag: active, .. }
            | CommandBlock::CaptureAlternate { tag: active, .. }
            | CommandBlock::CaptureMetadata { tag: active, .. } => *active == tag,
            CommandBlock::None => false,
        }
    }

    fn active_scope(&self) -> String {
        match &self.command_block {
            CommandBlock::Unknown {
                pane_id: Some(pane_id),
                ..
            }
            | CommandBlock::Resume { pane_id, .. }
            | CommandBlock::CapturePrimary { pane_id, .. }
            | CommandBlock::CaptureAlternate { pane_id, .. }
            | CommandBlock::CaptureMetadata { pane_id, .. } => pane_id.clone(),
            _ => "terminal".into(),
        }
    }

    pub(super) fn start_block(&mut self, tag: CommandTag) -> CommandBlock {
        if let Some(pane_id) = self.expected_resume.take() {
            CommandBlock::Resume {
                tag,
                pane_id,
                lines: Vec::new(),
            }
        } else if let Some(pane_id) = self.expected_capture.take() {
            self.pane_states.insert(
                pane_id.clone(),
                PaneSeedState::Pending {
                    buffered: Vec::new(),
                    buffered_bytes: 0,
                    overflowed: false,
                },
            );
            CommandBlock::CapturePrimary {
                tag,
                pane_id,
                lines: Vec::new(),
            }
        } else if let Some((pane_id, visible_lines, visible_boundary)) =
            self.pending_alternate.take()
        {
            CommandBlock::CaptureAlternate {
                tag,
                pane_id,
                visible_lines,
                visible_boundary,
                lines: Vec::new(),
            }
        } else if let Some(pending) = self.pending_metadata.take() {
            CommandBlock::CaptureMetadata {
                tag,
                pane_id: pending.pane_id,
                visible_lines: pending.visible_lines,
                saved_normal_lines: pending.saved_normal_lines,
                visible_boundary: pending.visible_boundary,
                lines: Vec::new(),
            }
        } else {
            CommandBlock::Unknown {
                tag,
                pane_id: None,
                lines: Vec::new(),
            }
        }
    }

    /// Asks for recovery of exactly the panes this control client owned.
    ///
    /// A pane-scoped resnapshot makes the desktop reseed that pane; an
    /// unscoped one makes it tear down and rebuild the whole bridge. Panes that
    /// disappeared along with their session simply have no seed to fetch, and
    /// the reconciler re-attaches the client if the session outlived it.
    fn emit_pane_scoped_recovery(
        &self,
        sender: &mpsc::Sender<SequencerControl>,
        overflowed: &AtomicBool,
        reason: &str,
    ) {
        if self.pane_states.is_empty() {
            emit_resnapshot(sender, overflowed, "terminal", reason.to_owned());
            return;
        }
        let mut pane_ids: Vec<_> = self.pane_states.keys().cloned().collect();
        pane_ids.sort();
        for pane_id in pane_ids {
            emit_resnapshot(sender, overflowed, &pane_id, reason.to_owned());
        }
    }
}

#[path = "stream_recovery.rs"]
mod stream_recovery;

#[path = "stream_helpers.rs"]
mod stream_helpers;
#[cfg(test)]
pub(in crate::service::terminal) use stream_helpers::OutputEmission as TestOutputEmission;
pub(super) use stream_helpers::with_active_resources;
use stream_helpers::{
    OutputEmission, emit_resnapshot, emit_terminal, is_topology_notification, notification_pane,
};

#[cfg(test)]
#[path = "stream_tests.rs"]
mod tests;
