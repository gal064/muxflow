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
use super::correlation::{
    MarkerBlock, classify_marker_block, error_reason, marker_pane, wants_error_line,
};
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
    let _ = writer.send(super::ControlWrite {
        pane_id: pane_id.to_owned(),
        resume_first,
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
}

pub(super) enum StreamControl {
    Membership {
        added: Vec<String>,
        removed: Vec<String>,
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
    } = context;
    let mut reader = BufReader::new(stdout);
    let mut parser = ControlParser::default();
    let mut state = StreamState::new(&pane_ids);
    let mut buffer = [0_u8; 64 * 1024];
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
                        Ok(record) => state.handle(
                            record,
                            StreamRuntime {
                                writer: &writer,
                                sender: &event_tx,
                                overflowed: &overflowed,
                                resources: &resources,
                                terminal_generation: &terminal_generation,
                                stopped: &stopped,
                            },
                        ),
                        Err(error) => {
                            emit_resnapshot(&event_tx, &overflowed, "terminal", error.to_string());
                            state.resnapshot_all(&writer);
                        }
                    }
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
    /// One in-band `send-keys` request, correlated to the pane its marker
    /// named. Input is fire-and-forget on the desktop side, so this block is
    /// the only place a rejected keystroke can still be attributed.
    Input {
        tag: CommandTag,
        pane_id: String,
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
    pub(super) expected_input: Option<String>,
    pub(super) expected_resume: Option<String>,
    pub(super) pending_alternate: Option<(String, Vec<Vec<u8>>, u64)>,
    pub(super) pending_metadata: Option<PendingCaptureMetadata>,
    command_block: CommandBlock,
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
}

impl StreamState {
    pub(super) fn new(pane_ids: &[String]) -> Self {
        Self {
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
            expected_input: None,
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
        } = runtime;
        match record {
            ControlRecord::Output { pane_id, data } => {
                let output_generation = terminal_generation.fetch_add(1, Ordering::AcqRel) + 1;
                match self.pane_states.get_mut(&pane_id) {
                    Some(PaneSeedState::Pending {
                        buffered,
                        buffered_bytes,
                        overflowed: replay_overflowed,
                    }) => {
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
                    _ => {
                        let disposition = resources.lock().unwrap().record_output(
                            &pane_id,
                            &data,
                            output_generation,
                        );
                        if disposition == OutputDisposition::Visible {
                            emit_terminal(
                                sender,
                                overflowed,
                                v1::EventKind::TerminalOutput,
                                pane_id,
                                data,
                                output_generation,
                            );
                        }
                    }
                }
            }
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
                CommandBlock::Input { lines, .. } | CommandBlock::Resume { lines, .. } => {
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
                sender,
                overflowed,
                resources,
                terminal_generation,
                writer,
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
                let detail = match &self.command_block {
                    CommandBlock::Input { pane_id, lines, .. } => format!(
                        "terminal input for {pane_id} was rejected by tmux: {}",
                        error_reason(&arguments, lines)
                    ),
                    CommandBlock::Resume { pane_id, lines, .. } => format!(
                        "tmux rejected the flow-control resume for {pane_id}; the pane stays paused until it is reseeded: {}",
                        error_reason(&arguments, lines)
                    ),
                    _ => arguments,
                };
                // An error abandons whatever multi-block sequence was running,
                // so every correlation slot has to be released too — otherwise
                // the next unrelated block is mistaken for the missing half of
                // this one.
                self.command_block = CommandBlock::None;
                self.expected_capture = None;
                self.expected_input = None;
                self.expected_resume = None;
                self.pending_alternate = None;
                self.pending_metadata = None;
                // No automatic retry here. The event above already asks the
                // desktop to reseed this pane, and a capture re-issued against a
                // pane that has just vanished fails at its own marker — which is
                // untargeted, so its failure would be attributed to the whole
                // connection and tear the bridge down.
                emit_resnapshot(sender, overflowed, &scope, detail);
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
                if let Some(pane_id) = arguments.split_whitespace().next()
                    && validate_tmux_id(pane_id, '%').is_ok()
                {
                    emit_event(
                        sender,
                        overflowed,
                        v1::HostEvent {
                            kind: v1::EventKind::TerminalFlowPaused.into(),
                            scope: pane_id.into(),
                            detail: "tmux pause-after flow control engaged".into(),
                            ..Default::default()
                        },
                    );
                    request_capture(writer, pane_id, true);
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
                )
            }
            _ => {}
        }
    }

    fn finish_block(
        &mut self,
        end_tag: CommandTag,
        sender: &mpsc::Sender<SequencerControl>,
        overflowed: &AtomicBool,
        resources: &Arc<Mutex<PaneResourceStore>>,
        terminal_generation: &Arc<AtomicU64>,
        writer: &std_mpsc::Sender<super::ControlWrite>,
    ) {
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
                    MarkerBlock::Input(pane_id) => self.expected_input = Some(pane_id),
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
            CommandBlock::Input { .. } | CommandBlock::Resume { .. } => {}
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
                                    let seed_generation =
                                        terminal_generation.fetch_add(1, Ordering::AcqRel) + 1;
                                    resources.lock().unwrap().snapshot(
                                        &pane_id,
                                        replay.seed.clone(),
                                        seed_generation,
                                    );
                                    let hidden = resources.lock().unwrap().is_hidden(&pane_id);
                                    if !hidden && !seed_build.diagnostics.is_empty() {
                                        emit_event(
                                            sender,
                                            overflowed,
                                            v1::HostEvent {
                                                kind: v1::EventKind::TerminalSeedDiagnostic.into(),
                                                scope: pane_id.clone(),
                                                detail: seed_build.diagnostics.join("; "),
                                                ..Default::default()
                                            },
                                        );
                                    }
                                    if !hidden {
                                        emit_terminal(
                                            sender,
                                            overflowed,
                                            v1::EventKind::TerminalSeed,
                                            pane_id.clone(),
                                            replay.seed,
                                            seed_generation,
                                        );
                                    }
                                    for output in replay.replay {
                                        // Buffered sequence numbers establish
                                        // capture inclusion only. Rebase
                                        // replay delivery after the seed so a
                                        // renderer's monotonic generation gate
                                        // cannot discard required output.
                                        let replay_generation =
                                            terminal_generation.fetch_add(1, Ordering::AcqRel) + 1;
                                        let disposition = resources.lock().unwrap().record_output(
                                            &pane_id,
                                            &output.bytes,
                                            replay_generation,
                                        );
                                        if disposition == OutputDisposition::Visible {
                                            emit_terminal(
                                                sender,
                                                overflowed,
                                                v1::EventKind::TerminalOutput,
                                                pane_id.clone(),
                                                output.bytes,
                                                replay_generation,
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
                    request_capture(writer, &pane_id, false);
                }
            }
            CommandBlock::None => {}
        }
    }

    fn active_tag_matches(&self, tag: CommandTag) -> bool {
        match &self.command_block {
            CommandBlock::Unknown { tag: active, .. }
            | CommandBlock::Input { tag: active, .. }
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
            | CommandBlock::Input { pane_id, .. }
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
        } else if let Some(pane_id) = self.expected_input.take() {
            CommandBlock::Input {
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

    fn apply_control(&mut self, control: StreamControl) {
        match control {
            StreamControl::Membership { added, removed } => {
                for pane_id in removed {
                    self.pane_states.remove(&pane_id);
                    if self.expected_capture.as_deref() == Some(&pane_id) {
                        self.expected_capture = None;
                    }
                    if self.expected_input.as_deref() == Some(&pane_id) {
                        self.expected_input = None;
                    }
                    if self.expected_resume.as_deref() == Some(&pane_id) {
                        self.expected_resume = None;
                    }
                    if self
                        .pending_alternate
                        .as_ref()
                        .is_some_and(|pending| pending.0 == pane_id)
                    {
                        self.pending_alternate = None;
                    }
                    if self
                        .pending_metadata
                        .as_ref()
                        .is_some_and(|pending| pending.pane_id == pane_id)
                    {
                        self.pending_metadata = None;
                    }
                    if self.active_scope() == pane_id {
                        self.command_block = CommandBlock::None;
                    }
                }
                for pane_id in added {
                    self.pane_states.insert(
                        pane_id,
                        PaneSeedState::Pending {
                            buffered: Vec::new(),
                            buffered_bytes: 0,
                            overflowed: false,
                        },
                    );
                }
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

    fn resnapshot_all(&mut self, writer: &std_mpsc::Sender<super::ControlWrite>) {
        self.expected_capture = None;
        self.expected_input = None;
        self.expected_resume = None;
        self.pending_alternate = None;
        self.pending_metadata = None;
        self.command_block = CommandBlock::None;
        for (pane_id, state) in &mut self.pane_states {
            *state = PaneSeedState::Pending {
                buffered: Vec::new(),
                buffered_bytes: 0,
                overflowed: false,
            };
            request_capture(writer, pane_id, false);
        }
    }
}

fn emit_resnapshot(
    sender: &mpsc::Sender<SequencerControl>,
    overflowed: &AtomicBool,
    scope: &str,
    reason: String,
) {
    emit_event(
        sender,
        overflowed,
        v1::HostEvent {
            kind: v1::EventKind::TerminalResnapshotRequired.into(),
            scope: scope.into(),
            detail: reason,
            ..Default::default()
        },
    );
}

fn emit_terminal(
    sender: &mpsc::Sender<SequencerControl>,
    overflowed: &AtomicBool,
    kind: v1::EventKind,
    pane_id: String,
    data: Vec<u8>,
    generation: u64,
) {
    if matches!(
        kind,
        v1::EventKind::TerminalSeed | v1::EventKind::TerminalOutput
    ) {
        let _ = super::super::agents::AgentRuntime::global().observe_screen(
            &pane_id,
            &data,
            kind == v1::EventKind::TerminalSeed,
        );
    }
    emit_event(
        sender,
        overflowed,
        v1::HostEvent {
            kind: kind.into(),
            terminal: Some(v1::TerminalBytes {
                pane_id,
                data,
                generation,
            }),
            ..Default::default()
        },
    );
}

fn is_topology_notification(name: &str) -> bool {
    matches!(
        name,
        "sessions-changed"
            | "session-changed"
            | "session-renamed"
            | "window-add"
            | "window-close"
            | "window-renamed"
            | "window-pane-changed"
            | "layout-change"
            | "window-linked"
            | "window-unlinked"
            | "pane-mode-changed"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pane_close_prunes_capture_state_without_disturbing_sibling() {
        let mut state = StreamState::new(&["%1".into(), "%2".into()]);
        state.command_block = CommandBlock::CapturePrimary {
            tag: CommandTag {
                timestamp: 1,
                number: 2,
                flags: 1,
            },
            pane_id: "%1".into(),
            lines: vec![b"stale".to_vec()],
        };
        state.pending_alternate = Some(("%1".into(), Vec::new(), 1));
        state.apply_control(StreamControl::Membership {
            added: Vec::new(),
            removed: vec!["%1".into()],
        });
        assert!(!state.pane_states.contains_key("%1"));
        assert!(state.pane_states.contains_key("%2"));
        assert!(matches!(state.command_block, CommandBlock::None));
        assert!(state.pending_alternate.is_none());
    }

    #[test]
    fn an_in_band_input_block_is_correlated_to_the_pane_that_was_typed_into() {
        // Input correlation is consumed by the block that follows its marker,
        // so an error inside that block recovers only the pane typed into.
        let mut state = StreamState::new(&["%1".into(), "%2".into()]);
        state.expected_input = Some("%2".into());
        let block = state.start_block(CommandTag {
            timestamp: 1,
            number: 2,
            flags: 1,
        });
        assert!(matches!(block, CommandBlock::Input { ref pane_id, .. } if pane_id == "%2"));
        state.command_block = block;
        assert_eq!(state.active_scope(), "%2");
    }

    #[test]
    fn capture_marker_carries_pane_scope() {
        let mut block = CommandBlock::Unknown {
            tag: CommandTag {
                timestamp: 1,
                number: 1,
                flags: 1,
            },
            pane_id: None,
            lines: Vec::new(),
        };
        if let CommandBlock::Unknown { pane_id, .. } = &mut block {
            *pane_id = marker_pane(b"__ADE_CAPTURE__:2");
        }
        let mut state = StreamState::new(&["%2".into()]);
        state.command_block = block;
        assert_eq!(state.active_scope(), "%2");
    }

    /// P12-U001's compounding bugs: a rejected `refresh-client -A` reported
    /// only the `%error` header's numbers (so the actual "parse error" was
    /// invisible in every log) and was attributed to the connection, which is
    /// what turned each one into a full resync — the P12-Q005 signature.
    #[test]
    fn a_rejected_resume_names_its_pane_and_carries_the_reason_tmux_printed() {
        let tag = CommandTag {
            timestamp: 1786682005,
            number: 425,
            flags: 1,
        };
        let mut state = StreamState::new(&["%1".into(), "%5".into()]);
        state.expected_resume = Some("%5".into());
        state.command_block = state.start_block(tag);
        assert!(matches!(
            state.command_block,
            CommandBlock::Resume { ref pane_id, .. } if pane_id == "%5"
        ));
        assert_eq!(state.active_scope(), "%5");

        let mut runtime_lines = Vec::new();
        if let CommandBlock::Resume { lines, .. } = &mut state.command_block {
            lines.push(b"parse error: syntax error".to_vec());
            runtime_lines.clone_from(lines);
        }
        let detail = error_reason("1786682005 425 1", &runtime_lines);
        assert_eq!(detail, "1786682005 425 1: parse error: syntax error");
        assert_eq!(error_reason("1786682005 425 1", &[]), "1786682005 425 1");
    }

    /// tmux emits `%continue` only when a pane really was paused and says
    /// nothing otherwise, so neither the notification nor a clean `%end` on the
    /// resume is an acknowledgement. Only the capture written with it restores
    /// the pane — output produced while paused is dropped, never replayed.
    #[test]
    fn a_clean_resume_block_is_not_treated_as_an_acknowledgement() {
        let tag = CommandTag {
            timestamp: 1,
            number: 7,
            flags: 1,
        };
        let mut state = StreamState::new(&["%5".into()]);
        state.expected_resume = Some("%5".into());
        state.command_block = state.start_block(tag);
        let (sender, _receiver) = mpsc::channel(8);
        let overflowed = AtomicBool::new(false);
        let resources = Arc::new(Mutex::new(PaneResourceStore::with_total_limit(
            4, 1024, 4096,
        )));
        let generation = Arc::new(AtomicU64::new(0));
        let (writer, _writes) = std_mpsc::channel();
        state.finish_block(tag, &sender, &overflowed, &resources, &generation, &writer);
        assert!(matches!(state.command_block, CommandBlock::None));
        assert!(
            matches!(
                state.pane_states.get("%5"),
                Some(PaneSeedState::Pending { .. })
            ),
            "a resume must not mark the pane live; its capture does that"
        );
    }
}
