//! One shown host's terminal path: the engine thread, what it produces, and
//! which pane it goes to. Port of the hub wiring in
//! `app/useAppConnectionController.ts`.
//!
//! The engine thread wakes nothing. Its outputs wait on a channel this entity
//! drains on a timer: a thread outside GPUI waking a GPUI task is what GPUI's
//! test scheduler rejects, and it would make this path unrunnable under test.
//! The poll is fast while there is output or input and one frame while idle,
//! which costs less than a keystroke's wait and nothing a user can see.

use std::collections::HashMap;
use std::sync::mpsc::{Receiver, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use gpui_kit::{Context, Entity, EventEmitter, SharedString, Task, WeakEntity};
use terminal_engine::{Command, EngineOutput, GridSize};

use crate::connection_ui::backend::{EngineChannel, TerminalBackend};

use super::paint_gate::PanePaintGate;
use super::pane::TerminalPane;
use super::probes::{GridMismatchProbe, PaintTail};

/// How often the channel is drained while output or input is moving. Well under
/// a frame, so a keystroke's echo reaches the next frame.
pub const POLL_ACTIVE: Duration = Duration::from_millis(4);
/// How often while nothing has moved for [`ACTIVE_FOR`]: one frame.
pub const POLL_IDLE: Duration = Duration::from_millis(16);
/// How long a burst of activity keeps the fast poll.
const ACTIVE_FOR: Duration = Duration::from_millis(500);

/// What a link owes the connection controller.
pub enum LinkEvent {
    /// Frames applied by the engine, as `(byte length, epoch)`. Only now does
    /// the host get its credit back.
    FramesConsumed(Vec<(usize, Option<u64>)>),
    FlowStalled {
        pane_id: String,
        epoch: Option<u64>,
    },
    Status(String),
    /// A program in a pane wrote the clipboard with OSC 52. Whether it may is
    /// the controller's call, which knows the host on screen.
    ClipboardWrite(String),
}

impl EventEmitter<LinkEvent> for TerminalLink {}

/// Shared by a link and its panes: an element paints without touching entities.
#[derive(Default)]
pub struct LinkProbes {
    pub tail: PaintTail,
    pub mismatch: GridMismatchProbe,
}

pub type SharedProbes = Arc<Mutex<LinkProbes>>;

pub struct TerminalLink {
    profile_id: String,
    engine: Arc<dyn EngineChannel>,
    backend: Arc<dyn TerminalBackend>,
    client_id: Option<String>,
    /// Whose host-epoch counts as the current bridge's.
    connection_epoch: u64,
    panes: HashMap<String, WeakEntity<TerminalPane>>,
    active_until: Instant,
    probes: SharedProbes,
    _poll: Task<()>,
}

impl TerminalLink {
    pub fn new(
        profile_id: String,
        backend: Arc<dyn TerminalBackend>,
        cx: &mut Context<Self>,
    ) -> Self {
        let (outputs, drained) = channel();
        let engine: Arc<dyn EngineChannel> = backend.spawn_engine(outputs).into();
        Self {
            profile_id,
            engine,
            backend,
            client_id: None,
            connection_epoch: 0,
            panes: HashMap::new(),
            active_until: Instant::now(),
            probes: Arc::default(),
            _poll: start_poll(drained, cx),
        }
    }

    pub fn profile_id(&self) -> &str {
        &self.profile_id
    }

    pub fn probes(&self) -> SharedProbes {
        self.probes.clone()
    }

    pub fn backend(&self) -> &Arc<dyn TerminalBackend> {
        &self.backend
    }

    pub fn client_id(&self) -> Option<&str> {
        self.client_id.as_deref()
    }

    pub fn connection_epoch(&self) -> u64 {
        self.connection_epoch
    }

    pub fn send(&self, command: Command) {
        // The thread is gone only if the engine panicked; there is no one to
        // tell here and nothing that would help.
        let _ = self.engine.send(command);
    }

    /// A frame from the bridge, in stream order. Its credit waits for the engine.
    pub fn forward_frame(&self, frame: Vec<u8>) {
        self.send(Command::Frame(frame));
    }

    pub fn set_client(&mut self, client_id: Option<String>, connection_epoch: u64) {
        self.client_id = client_id.clone();
        self.connection_epoch = connection_epoch;
        self.send(Command::SetClient(client_id));
    }

    pub fn mount(&mut self, pane_id: &str, pane: &Entity<TerminalPane>, size: GridSize) {
        self.panes.insert(pane_id.to_owned(), pane.downgrade());
        self.send(Command::Mount {
            pane_id: pane_id.to_owned(),
            size,
        });
    }

    pub fn unmount(&mut self, pane_id: &str) {
        self.panes.remove(pane_id);
        lock(&self.probes).mismatch.clear(pane_id);
        self.send(Command::Unmount {
            pane_id: pane_id.to_owned(),
        });
    }

    pub fn set_grid(&self, pane_id: &str, size: GridSize) {
        self.send(Command::SetGrid {
            pane_id: pane_id.to_owned(),
            size,
        });
    }

    /// A key or a paste went out, so its echo is coming.
    pub fn note_input(&mut self) {
        self.active_until = Instant::now() + ACTIVE_FOR;
    }

    fn poll_interval(&self, now: Instant) -> Duration {
        if now < self.active_until {
            POLL_ACTIVE
        } else {
            POLL_IDLE
        }
    }

    fn pane(&self, pane_id: &str) -> Option<Entity<TerminalPane>> {
        self.panes.get(pane_id)?.upgrade()
    }

    fn with_pane(
        &self,
        pane_id: &str,
        cx: &mut Context<Self>,
        update: impl FnOnce(&mut TerminalPane, &mut Context<TerminalPane>),
    ) {
        if let Some(pane) = self.pane(pane_id) {
            pane.update(cx, update);
        }
    }

    fn scoped_key(&self, pane_id: &str) -> String {
        format!("{}\0{}", self.profile_id, pane_id)
    }

    /// Runs what the engine produced, in order.
    fn apply(&mut self, outputs: Vec<EngineOutput>, cx: &mut Context<Self>) {
        if outputs.is_empty() {
            return;
        }
        self.active_until = Instant::now() + ACTIVE_FOR;
        let mut consumed = Vec::new();
        for output in outputs {
            match output {
                EngineOutput::FrameConsumed { byte_length, epoch } => {
                    consumed.push((byte_length, epoch));
                }
                EngineOutput::Dirty { pane_id } => {
                    self.with_pane(&pane_id, cx, |pane, cx| pane.on_dirty(cx));
                }
                other => self.apply_one(other, cx),
            }
        }
        if !consumed.is_empty() {
            cx.emit(LinkEvent::FramesConsumed(consumed));
        }
    }

    fn apply_one(&mut self, output: EngineOutput, cx: &mut Context<Self>) {
        match output {
            EngineOutput::Mounted { pane_id, terminal } => {
                let key: SharedString = self.scoped_key(&pane_id).into();
                self.with_pane(&pane_id, cx, |pane, cx| {
                    pane.on_mounted(terminal, key, cx);
                });
            }
            EngineOutput::Revealed { pane_id } => {
                self.with_pane(&pane_id, cx, |pane, cx| pane.on_revealed(cx));
            }
            EngineOutput::GridApplied { pane_id, size } => {
                self.with_pane(&pane_id, cx, |pane, cx| pane.on_grid_applied(size, cx));
            }
            EngineOutput::ReadingChanged { pane_id, state } => {
                self.with_pane(&pane_id, cx, |pane, cx| pane.on_reading_changed(state, cx));
            }
            EngineOutput::Repaint {
                pane_id,
                sequence,
                generation,
            } => {
                self.with_pane(&pane_id, cx, |pane, _| {
                    pane.input.note_output(sequence, generation);
                });
            }
            EngineOutput::RendererDiagnostic { pane_id, message } => {
                self.with_pane(&pane_id, cx, |pane, cx| {
                    pane.set_renderer_diagnostic(message, cx);
                });
            }
            EngineOutput::SeedDiagnostic { pane_id, message } => {
                self.with_pane(&pane_id, cx, |pane, cx| {
                    pane.set_seed_diagnostic(message, cx)
                });
            }
            EngineOutput::Diagnostic { message, .. } => cx.emit(LinkEvent::Status(message)),
            EngineOutput::Incident { kind, detail } => self.backend.record_incident(kind, detail),
            EngineOutput::ClipboardWrite { text, .. } => cx.emit(LinkEvent::ClipboardWrite(text)),
            EngineOutput::FlowStalled { pane_id, epoch } => {
                cx.emit(LinkEvent::FlowStalled { pane_id, epoch });
            }
            EngineOutput::RequestSeed {
                client_id,
                pane_id,
                reason,
            } => self.request_seed(client_id, pane_id, reason, cx),
            EngineOutput::RequestHistory {
                client_id,
                pane_id,
                lines,
                skip,
                serial,
            } => self.request_history(client_id, pane_id, lines, skip, serial, cx),
            EngineOutput::SetVisibility {
                client_id,
                pane_id,
                visible,
                holds_snapshot,
                checkpoint,
                token,
            } => {
                let revealed = self
                    .pane(&pane_id)
                    .is_some_and(|pane| pane.read(cx).revealed());
                if visible && !revealed {
                    cx.default_global::<PanePaintGate>()
                        .arm(&self.scoped_key(&pane_id));
                }
                let answer = self.backend.set_pane_visibility(
                    client_id,
                    pane_id,
                    visible,
                    holds_snapshot,
                    checkpoint.terminal_epoch,
                    checkpoint.output_generation,
                );
                cx.spawn(async move |this, cx| {
                    let result = answer.await;
                    let _ = this.update(cx, |link, _| {
                        link.send(Command::VisibilityAnswered { token, result });
                    });
                })
                .detach();
            }
            EngineOutput::Epoch(_)
            | EngineOutput::Dirty { .. }
            | EngineOutput::FrameConsumed { .. } => {}
        }
    }

    fn request_seed(
        &self,
        client_id: String,
        pane_id: String,
        reason: String,
        cx: &mut Context<Self>,
    ) {
        let answer = self.backend.request_seed(client_id, pane_id.clone());
        cx.spawn(async move |this, cx| {
            if let Err(error) = answer.await {
                let _ = this.update(cx, |link, _| {
                    link.send(Command::SeedRequestFailed {
                        pane_id,
                        reason,
                        error,
                    });
                });
            }
        })
        .detach();
    }

    fn request_history(
        &self,
        client_id: String,
        pane_id: String,
        lines: u32,
        skip: usize,
        serial: u64,
        cx: &mut Context<Self>,
    ) {
        let answer = self
            .backend
            .request_history(client_id, pane_id.clone(), lines, skip as u32);
        cx.spawn(async move |this, cx| {
            if let Err(error) = answer.await {
                let _ = this.update(cx, |link, _| {
                    link.send(Command::HistoryRequestFailed {
                        pane_id,
                        serial,
                        error,
                    });
                });
            }
        })
        .detach();
    }

    fn flush_probes(&self, now: Instant) {
        let due = lock(&self.probes).mismatch.poll(now);
        for detail in due {
            self.backend.record_incident("pane.gridMismatch", detail);
        }
    }
}

pub fn lock(probes: &SharedProbes) -> std::sync::MutexGuard<'_, LinkProbes> {
    probes
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn start_poll(drained: Receiver<EngineOutput>, cx: &mut Context<TerminalLink>) -> Task<()> {
    cx.spawn(async move |this, cx| {
        let mut last = Instant::now();
        let mut interval = POLL_IDLE;
        loop {
            let now = Instant::now();
            let stall = now.saturating_duration_since(last).saturating_sub(interval);
            last = now;
            let outputs: Vec<EngineOutput> = drained.try_iter().collect();
            let updated = this.update(cx, |link, cx| {
                lock(&link.probes).tail.note_stall(stall, now);
                link.apply(outputs, cx);
                link.flush_probes(now);
                link.poll_interval(now)
            });
            let Ok(next) = updated else {
                return;
            };
            interval = next;
            cx.background_executor().timer(interval).await;
        }
    })
}
