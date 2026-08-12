use std::{
    collections::HashMap,
    io::{BufReader, Read, Write},
    process::{ChildStdin, ChildStdout},
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
use super::{
    build_seed_with_metadata, capture_metadata, queue_capture, selected_capture_boundary,
    validate_tmux_id,
};

pub(super) struct ControlStreamReader {
    pub(super) stdout: ChildStdout,
    pub(super) stdin: Arc<Mutex<ChildStdin>>,
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
        stdin,
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
                                stdin: &stdin,
                                sender: &event_tx,
                                overflowed: &overflowed,
                                resources: &resources,
                                terminal_generation: &terminal_generation,
                                stopped: &stopped,
                            },
                        ),
                        Err(error) => {
                            emit_resnapshot(&event_tx, &overflowed, "terminal", error.to_string());
                            state.resnapshot_all(&stdin);
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
        emit_resnapshot(
            &event_tx,
            &overflowed,
            "terminal",
            "tmux session control stream ended".into(),
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
    CapturePrimary {
        tag: CommandTag,
        pane_id: String,
        lines: Vec<Vec<u8>>,
    },
    CaptureAlternate {
        tag: CommandTag,
        pane_id: String,
        primary_lines: Vec<Vec<u8>>,
        primary_boundary: u64,
        lines: Vec<Vec<u8>>,
    },
    CaptureMetadata {
        tag: CommandTag,
        pane_id: String,
        primary_lines: Vec<Vec<u8>>,
        alternate_lines: Vec<Vec<u8>>,
        primary_boundary: u64,
        alternate_boundary: u64,
        lines: Vec<Vec<u8>>,
    },
}

pub(super) struct StreamState {
    pub(super) pane_states: HashMap<String, PaneSeedState>,
    pub(super) expected_capture: Option<String>,
    pub(super) pending_alternate: Option<(String, Vec<Vec<u8>>, u64)>,
    pub(super) pending_metadata: Option<PendingCaptureMetadata>,
    command_block: CommandBlock,
}

pub(super) struct PendingCaptureMetadata {
    pub(super) pane_id: String,
    pub(super) primary_lines: Vec<Vec<u8>>,
    pub(super) alternate_lines: Vec<Vec<u8>>,
    pub(super) primary_boundary: u64,
    pub(super) alternate_boundary: u64,
}

struct StreamRuntime<'a> {
    stdin: &'a Arc<Mutex<ChildStdin>>,
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
            pending_alternate: None,
            pending_metadata: None,
            command_block: CommandBlock::None,
        }
    }

    fn handle(&mut self, record: ControlRecord, runtime: StreamRuntime<'_>) {
        let StreamRuntime {
            stdin,
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
            ControlRecord::End { tag, .. } => {
                self.finish_block(tag, sender, overflowed, resources, terminal_generation)
            }
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
                self.command_block = CommandBlock::None;
                emit_resnapshot(sender, overflowed, &scope, arguments);
            }
            ControlRecord::Exit { reason } => {
                if !stopped.load(Ordering::Acquire) {
                    emit_resnapshot(sender, overflowed, "terminal", reason);
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
                    if let Ok(mut writer) = stdin.lock() {
                        let _ = writeln!(writer, "refresh-client -A {pane_id}:continue");
                        let _ = queue_capture(&mut writer, pane_id);
                        let _ = writer.flush();
                    }
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
                let pane_id = pane_id.or_else(|| lines.iter().find_map(|line| marker_pane(line)));
                self.expected_capture =
                    pane_id.filter(|pane_id| self.pane_states.contains_key(pane_id));
            }
            CommandBlock::CapturePrimary { pane_id, lines, .. } => {
                // tmux emits one %begin/%end block per command separated by
                // `;`: capture-pane and its following display-message metadata
                // are distinct correlated blocks.
                let primary_boundary = terminal_generation.load(Ordering::Acquire);
                self.pending_alternate = Some((pane_id, lines, primary_boundary));
            }
            CommandBlock::CaptureAlternate {
                pane_id,
                primary_lines,
                primary_boundary,
                lines,
                ..
            } => {
                // This is the precise output boundary represented by both
                // screen captures. Later terminal output is replayed once.
                let alternate_boundary = terminal_generation.load(Ordering::Acquire);
                self.pending_metadata = Some(PendingCaptureMetadata {
                    pane_id,
                    primary_lines,
                    alternate_lines: lines,
                    primary_boundary,
                    alternate_boundary,
                });
            }
            CommandBlock::CaptureMetadata {
                pane_id,
                primary_lines,
                alternate_lines,
                primary_boundary,
                alternate_boundary,
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
                                let capture_boundary = selected_capture_boundary(
                                    metadata,
                                    primary_boundary,
                                    alternate_boundary,
                                );
                                let seed_build = build_seed_with_metadata(
                                    primary_lines,
                                    alternate_lines,
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
                }
            }
            CommandBlock::None => {}
        }
    }

    fn active_tag_matches(&self, tag: CommandTag) -> bool {
        match &self.command_block {
            CommandBlock::Unknown { tag: active, .. }
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
            | CommandBlock::CapturePrimary { pane_id, .. }
            | CommandBlock::CaptureAlternate { pane_id, .. }
            | CommandBlock::CaptureMetadata { pane_id, .. } => pane_id.clone(),
            _ => "terminal".into(),
        }
    }

    pub(super) fn start_block(&mut self, tag: CommandTag) -> CommandBlock {
        if let Some(pane_id) = self.expected_capture.take() {
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
        } else if let Some((pane_id, primary_lines, primary_boundary)) =
            self.pending_alternate.take()
        {
            CommandBlock::CaptureAlternate {
                tag,
                pane_id,
                primary_lines,
                primary_boundary,
                lines: Vec::new(),
            }
        } else if let Some(pending) = self.pending_metadata.take() {
            CommandBlock::CaptureMetadata {
                tag,
                pane_id: pending.pane_id,
                primary_lines: pending.primary_lines,
                alternate_lines: pending.alternate_lines,
                primary_boundary: pending.primary_boundary,
                alternate_boundary: pending.alternate_boundary,
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

    fn resnapshot_all(&mut self, stdin: &Arc<Mutex<ChildStdin>>) {
        self.expected_capture = None;
        self.pending_alternate = None;
        self.pending_metadata = None;
        self.command_block = CommandBlock::None;
        if let Ok(mut writer) = stdin.lock() {
            for (pane_id, state) in &mut self.pane_states {
                *state = PaneSeedState::Pending {
                    buffered: Vec::new(),
                    buffered_bytes: 0,
                    overflowed: false,
                };
                let _ = queue_capture(&mut writer, pane_id);
            }
            let _ = writer.flush();
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

fn marker_pane(line: &[u8]) -> Option<String> {
    let pane = std::str::from_utf8(line.strip_prefix(b"__ADE_CAPTURE__:")?)
        .ok()?
        .to_owned();
    validate_tmux_id(&pane, '%').ok()?;
    Some(pane)
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
            *pane_id = marker_pane(b"__ADE_CAPTURE__:%2");
        }
        let mut state = StreamState::new(&["%2".into()]);
        state.command_block = block;
        assert_eq!(state.active_scope(), "%2");
    }
}
