//! One host's terminals: the hub, the mounted panes and the screens kept for
//! hidden ones. Sans-IO. [`spawn`] is the only part that touches a thread or a
//! clock.

use std::collections::{BTreeMap, HashMap};
use std::mem::size_of;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use alacritty_terminal::term::cell::Cell;
use indexmap::IndexMap;
use serde_json::json;

use crate::event::{GridSize, PaneEvent, TerminalEvent, decode_frame};
use crate::hub::{Admission, HubLimits, HubOutput, TerminalEventHub};
use crate::pager::{CachedHistoryState, HistoryPageTrigger};
use crate::pane::{EngineOutput, Held, Pane, PendingHandoff, Shared};
use crate::reveal_retry::is_quiet_reveal_error;
use crate::terminal::{PaneWriter, SCROLLBACK_LIMIT};

const MAX_RETAINED_SCREENS: usize = 64;

/// Bounds screens kept live for hidden panes. The desktop's 24 MiB bounded
/// serialized text; a live grid costs a cell struct per column.
const MAX_RETAINED_BYTES: usize = 128 * 1024 * 1024;

/// How long an idle engine sleeps when nothing has a deadline.
const IDLE_WAIT: Duration = Duration::from_secs(3600);

pub(crate) struct RetainedScreen {
    writer: PaneWriter,
    terminal_epoch: u64,
    output_generation: u64,
    history: CachedHistoryState,
    byte_length: usize,
}

#[derive(Debug)]
pub enum Command {
    /// One encoded host event frame, in stream order.
    Frame(Vec<u8>),
    Mount {
        pane_id: String,
        size: GridSize,
    },
    Unmount {
        pane_id: String,
    },
    SetGrid {
        pane_id: String,
        size: GridSize,
    },
    RequestPage {
        pane_id: String,
        trigger: HistoryPageTrigger,
    },
    Viewport {
        pane_id: String,
        at_bottom: bool,
    },
    ReturnToLive {
        pane_id: String,
        reason: String,
    },
    SetClient(Option<String>),
    VisibilityAnswered {
        token: u64,
        result: Result<(), String>,
    },
    SeedRequestFailed {
        pane_id: String,
        reason: String,
        error: String,
    },
    HistoryRequestFailed {
        pane_id: String,
        serial: u64,
        error: String,
    },
    /// A resync restored ordering but not content.
    ReseedMounted(String),
    /// The host's server identity changed: nothing kept here describes it.
    ClearTerminalState,
    Stop,
}

/// The state of one host's terminal streams.
pub struct Engine {
    shared: Shared,
    panes: BTreeMap<String, Pane>,
    retained: IndexMap<String, RetainedScreen>,
    retained_bytes: usize,
    /// The pane a flow-stall frame being published is about.
    stalled_pane: Option<String>,
}

impl Default for Engine {
    fn default() -> Self {
        Self::new(HubLimits::default())
    }
}

impl Engine {
    pub fn new(limits: HubLimits) -> Self {
        Self {
            shared: Shared {
                hub: TerminalEventHub::new(limits),
                hub_out: Vec::new(),
                out: Vec::new(),
                client_id: None,
                handoffs: HashMap::new(),
                next_token: 0,
            },
            panes: BTreeMap::new(),
            retained: IndexMap::new(),
            retained_bytes: 0,
            stalled_pane: None,
        }
    }

    pub fn hub(&self) -> &TerminalEventHub {
        &self.shared.hub
    }

    pub fn pane_terminal(
        &self,
        pane_id: &str,
    ) -> Option<&std::sync::Arc<crate::terminal::PaneTerminal>> {
        self.panes.get(pane_id).map(|pane| pane.writer.terminal())
    }

    pub fn retained_screen_count(&self) -> usize {
        self.retained.len()
    }

    /// Handles one command and returns what it produced, in order. `Stop` is
    /// `spawn`'s to act on and does nothing here.
    pub fn handle(&mut self, command: Command, now: Instant) -> Vec<EngineOutput> {
        match command {
            Command::Frame(bytes) => self.frame(&bytes, now),
            Command::Mount { pane_id, size } => self.mount(pane_id, size, now),
            Command::Unmount { pane_id } => self.unmount(&pane_id, now),
            Command::SetGrid { pane_id, size } => {
                if let Some(pane) = self.panes.get_mut(&pane_id) {
                    pane.ui_grid(size, &mut self.shared);
                }
            }
            Command::RequestPage { pane_id, trigger } => {
                if let Some(pane) = self.panes.get_mut(&pane_id) {
                    pane.request_page(trigger, &mut self.shared);
                }
            }
            Command::Viewport { pane_id, at_bottom } => {
                if let Some(pane) = self.panes.get_mut(&pane_id) {
                    pane.viewport_changed(at_bottom, &mut self.shared, now);
                }
            }
            Command::ReturnToLive { pane_id, reason } => {
                if let Some(pane) = self.panes.get_mut(&pane_id) {
                    pane.resume_live(&reason, &mut self.shared, now);
                }
            }
            Command::SetClient(client_id) => self.set_client(client_id, now),
            Command::VisibilityAnswered { token, result } => {
                self.visibility_answered(token, result, now)
            }
            Command::SeedRequestFailed {
                pane_id,
                reason,
                error,
            } => self.seed_request_failed(pane_id, reason, error),
            Command::HistoryRequestFailed {
                pane_id,
                serial,
                error,
            } => {
                if let Some(pane) = self.panes.get_mut(&pane_id) {
                    pane.history_request_failed(serial, &error, &mut self.shared);
                }
            }
            Command::ReseedMounted(reason) => {
                self.shared
                    .hub
                    .reseed_subscribed_panes(&reason, &mut self.shared.hub_out);
            }
            Command::ClearTerminalState => {
                self.clear_retained();
                self.shared
                    .hub
                    .clear_terminal_state(&mut self.shared.hub_out);
            }
            Command::Stop => {}
        }
        self.pump(now);
        std::mem::take(&mut self.shared.out)
    }

    /// Runs everything due at `now`.
    pub fn poll(&mut self, now: Instant) -> Vec<EngineOutput> {
        let waited_on: Vec<String> = self.panes.keys().cloned().collect();
        for pane_id in waited_on {
            if let Some(pane) = self.panes.get_mut(&pane_id) {
                pane.poll(&mut self.shared, now);
            }
        }
        self.pump(now);
        std::mem::take(&mut self.shared.out)
    }

    pub fn next_deadline(&self) -> Option<Instant> {
        self.panes.values().filter_map(Pane::next_deadline).min()
    }

    fn frame(&mut self, bytes: &[u8], now: Instant) {
        match decode_frame(bytes) {
            Ok(event) => self.publish_event(event, now),
            // The sequence hole this leaves is the hub's to notice and repair.
            Err(error) => self.shared.out.push(EngineOutput::Incident {
                kind: "link.decodeFailure",
                detail: json!({ "message": error }),
            }),
        }
        self.pump(now);
        self.shared.out.push(EngineOutput::FrameConsumed {
            byte_length: bytes.len(),
            epoch: self.shared.hub.generation_epoch(),
        });
    }

    fn publish_event(&mut self, event: TerminalEvent, now: Instant) {
        if let TerminalEvent::FlowStalled { pane_id, .. } = &event {
            self.stalled_pane = Some(pane_id.clone());
        }
        let admission = self
            .shared
            .hub
            .publish(event, now, &mut self.shared.hub_out);
        if admission == Admission::Stale {
            self.stalled_pane = None;
        }
    }

    fn mount(&mut self, pane_id: String, size: GridSize, now: Instant) {
        if self.panes.contains_key(&pane_id) {
            self.shared.out.push(EngineOutput::Diagnostic {
                pane_id: pane_id.clone(),
                message: format!("terminal pane {pane_id} already has an active consumer"),
            });
            return;
        }
        let epoch = self.shared.hub.generation_epoch();
        let retained = self
            .take_retained(&pane_id)
            .filter(|screen| Some(screen.terminal_epoch) == epoch);
        let (writer, kept) = match retained {
            Some(screen) => {
                let held = Held {
                    terminal_epoch: screen.terminal_epoch,
                    output_generation: screen.output_generation,
                };
                (screen.writer, Some((held, screen.history)))
            }
            None => (PaneWriter::new(size), None),
        };
        self.shared.out.push(EngineOutput::Mounted {
            pane_id: pane_id.clone(),
            terminal: writer.terminal().clone(),
        });
        let mut pane = match kept {
            Some((held, history)) => {
                Pane::restored(&pane_id, writer, held, history, &mut self.shared)
            }
            None => Pane::new(&pane_id, writer),
        };
        pane.set_grid(size, &mut self.shared);
        self.panes.insert(pane_id.clone(), pane);
        if let Err(message) = self
            .shared
            .hub
            .subscribe_pane(&pane_id, &mut self.shared.hub_out)
        {
            self.shared.out.push(EngineOutput::Diagnostic {
                pane_id: pane_id.clone(),
                message,
            });
            return;
        }
        self.pump(now);
        if let Some(pane) = self.panes.get_mut(&pane_id) {
            pane.reveal_for_current_epoch(&mut self.shared, now);
        }
    }

    fn unmount(&mut self, pane_id: &str, now: Instant) {
        let Some(mut pane) = self.panes.remove(pane_id) else {
            return;
        };
        self.shared
            .hub
            .unsubscribe_pane(pane_id, &mut self.shared.hub_out);
        let handoff = pane.begin_hide();
        let Some(mut checkpoint) = self.shared.hub.visibility_checkpoint(pane_id) else {
            return;
        };
        let matches_epoch = handoff.renderer_epoch == Some(checkpoint.terminal_epoch);
        checkpoint.output_generation = if matches_epoch {
            handoff.output_generation
        } else {
            0
        };
        self.drop_retained(pane_id);
        let mut holds_snapshot = false;
        if matches_epoch && handoff.screen_is_current {
            let facts = pane.writer.facts();
            // The grid in use on the alternate screen has no history, and the one
            // holding it cannot be measured. Charge what it can hold.
            let scrollback = if facts.alternate_screen {
                SCROLLBACK_LIMIT
            } else {
                facts.scrollback_rows
            };
            let rows = scrollback + facts.grid.rows;
            let byte_length = rows * facts.grid.columns * size_of::<Cell>();
            if byte_length <= MAX_RETAINED_BYTES {
                self.retain(
                    pane_id,
                    RetainedScreen {
                        writer: pane.writer,
                        terminal_epoch: checkpoint.terminal_epoch,
                        output_generation: checkpoint.output_generation,
                        history: handoff.history,
                        byte_length,
                    },
                );
                holds_snapshot = true;
            }
        }
        let Some(client_id) = self.shared.client_id.clone() else {
            return;
        };
        let token = self.shared.token();
        self.shared.handoffs.insert(
            pane_id.to_owned(),
            PendingHandoff {
                token,
                client_id: client_id.clone(),
                terminal_epoch: checkpoint.terminal_epoch,
                timed_out: false,
            },
        );
        self.shared.out.push(EngineOutput::SetVisibility {
            client_id,
            pane_id: pane_id.to_owned(),
            visible: false,
            holds_snapshot,
            checkpoint,
            token,
        });
        self.pump(now);
    }

    fn set_client(&mut self, client_id: Option<String>, now: Instant) {
        self.shared.client_id = client_id;
        for pane in self.panes.values_mut() {
            pane.client_changed();
            pane.reveal_for_current_epoch(&mut self.shared, now);
        }
    }

    fn visibility_answered(&mut self, token: u64, result: Result<(), String>, now: Instant) {
        let finished = self
            .shared
            .handoffs
            .iter()
            .find(|(_, handoff)| handoff.token == token)
            .map(|(pane_id, _)| pane_id.clone());
        if let Some(pane_id) = finished {
            let handoff = self.shared.handoffs.remove(&pane_id).expect("just found");
            if let Err(error) = &result {
                self.shared.out.push(EngineOutput::Diagnostic {
                    pane_id: pane_id.clone(),
                    message: format!("Could not mark {pane_id} hidden: {error}"),
                });
                // A rejected cutoff is never adjusted or guessed.
                if self.shared.client_id.as_deref() == Some(handoff.client_id.as_str())
                    && self.shared.hub.generation_epoch() == Some(handoff.terminal_epoch)
                {
                    self.shared
                        .request_seed(&pane_id, "the host refused this pane's hide checkpoint");
                }
            }
            if let Some(pane) = self.panes.get_mut(&pane_id) {
                pane.handoff_finished(&mut self.shared);
            }
            return;
        }
        for pane in self.panes.values_mut() {
            if pane.reveal_answered(token, &result, &mut self.shared, now) {
                return;
            }
        }
    }

    fn seed_request_failed(&mut self, pane_id: String, reason: String, error: String) {
        self.shared.out.push(EngineOutput::Incident {
            kind: "pane.reseedFailed",
            detail: json!({
                "paneId": pane_id,
                "error": error.chars().take(200).collect::<String>(),
            }),
        });
        // A transport still coming up is journal material: the watchdog retries.
        if !is_quiet_reveal_error(&error) {
            self.shared.out.push(EngineOutput::Diagnostic {
                message: format!("{reason}; fresh seed request failed: {error}"),
                pane_id,
            });
        }
    }

    /// Hub outputs in the order the hub produced them. Applying one can queue
    /// more, which then run after everything already queued.
    fn pump(&mut self, now: Instant) {
        while !self.shared.hub_out.is_empty() {
            for output in std::mem::take(&mut self.shared.hub_out) {
                self.apply(output, now);
            }
        }
    }

    fn apply(&mut self, output: HubOutput, now: Instant) {
        match output {
            HubOutput::DeliverToApp => {
                if let Some(pane_id) = self.stalled_pane.take() {
                    self.drop_retained(&pane_id);
                    if let Some(pane) = self.panes.get_mut(&pane_id) {
                        pane.held = None;
                    }
                    self.shared.out.push(EngineOutput::FlowStalled { pane_id });
                }
            }
            HubOutput::Epoch(epoch) => {
                self.clear_retained();
                self.shared.out.push(EngineOutput::Epoch(epoch));
                for pane in self.panes.values_mut() {
                    pane.epoch_changed(&mut self.shared, now);
                }
            }
            HubOutput::Deliver(event) => self.deliver(event, now),
            HubOutput::Grid { pane_id, size } => {
                let applied = self.panes.get_mut(&pane_id).map(|pane| {
                    catch_unwind(AssertUnwindSafe(|| {
                        pane.stream_grid(size, &mut self.shared)
                    }))
                });
                if let Some(Err(_)) = applied {
                    self.require_seed(&pane_id, "terminal pane rejected its ordered grid");
                }
            }
            HubOutput::Health { pane_id, health } => {
                if let Some(pane) = self.panes.get_mut(&pane_id) {
                    pane.health_changed(health, now);
                }
            }
            HubOutput::SeedRequired { pane_id, reason } => {
                self.drop_retained(&pane_id);
                if let Some(pane) = self.panes.get_mut(&pane_id) {
                    pane.held = None;
                }
                self.shared.request_seed(&pane_id, &reason);
            }
            HubOutput::Repaint {
                pane_id,
                sequence,
                generation,
            } => self.shared.out.push(EngineOutput::Repaint {
                pane_id,
                sequence,
                generation,
            }),
            HubOutput::Incident { kind, detail } => {
                self.shared
                    .out
                    .push(EngineOutput::Incident { kind, detail });
            }
        }
    }

    fn deliver(&mut self, event: PaneEvent, now: Instant) {
        let pane_id = event.pane_id().to_owned();
        let reason = if matches!(event, PaneEvent::History { .. }) {
            "terminal pane consumer rejected a history answer"
        } else {
            "terminal pane consumer rejected an admitted event"
        };
        let Some(pane) = self.panes.get_mut(&pane_id) else {
            return;
        };
        // A panic inside the parser leaves a screen nobody can vouch for. A seed
        // starts it over; the rest of this host's panes never notice.
        let applied = catch_unwind(AssertUnwindSafe(|| {
            pane.apply(event, &mut self.shared, now)
        }));
        if applied.is_err() {
            self.require_seed(&pane_id, reason);
        }
    }

    fn require_seed(&mut self, pane_id: &str, reason: &str) {
        self.shared
            .hub
            .require_seed(pane_id, reason, &mut self.shared.hub_out);
    }

    fn take_retained(&mut self, pane_id: &str) -> Option<RetainedScreen> {
        let screen = self.retained.shift_remove(pane_id)?;
        self.retained_bytes -= screen.byte_length;
        Some(screen)
    }

    fn drop_retained(&mut self, pane_id: &str) {
        self.take_retained(pane_id);
    }

    fn clear_retained(&mut self) {
        self.retained.clear();
        self.retained_bytes = 0;
    }

    fn retain(&mut self, pane_id: &str, screen: RetainedScreen) {
        self.retained_bytes += screen.byte_length;
        self.retained.insert(pane_id.to_owned(), screen);
        while self.retained.len() > MAX_RETAINED_SCREENS || self.retained_bytes > MAX_RETAINED_BYTES
        {
            let Some((_, oldest)) = self.retained.shift_remove_index(0) else {
                break;
            };
            self.retained_bytes -= oldest.byte_length;
        }
    }
}

pub struct EngineHandle {
    commands: Sender<Command>,
    thread: Option<JoinHandle<()>>,
}

impl EngineHandle {
    /// False once the engine thread has gone.
    pub fn send(&self, command: Command) -> bool {
        self.commands.send(command).is_ok()
    }

    pub fn is_finished(&self) -> bool {
        self.thread.as_ref().is_none_or(JoinHandle::is_finished)
    }
}

impl Drop for EngineHandle {
    fn drop(&mut self) {
        let _ = self.commands.send(Command::Stop);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Runs an [`Engine`] on its own thread. The inbox is unbounded on purpose: the
/// host stops sending past its delivery window, and a frame is only acknowledged
/// after it has been applied, so the window is the bound.
pub fn spawn(limits: HubLimits, outputs: Sender<EngineOutput>) -> EngineHandle {
    let (commands, inbox) = mpsc::channel();
    let thread = std::thread::Builder::new()
        .name("terminal-engine".into())
        .spawn(move || {
            let mut engine = Engine::new(limits);
            loop {
                let now = Instant::now();
                let wait = engine.next_deadline().map_or(IDLE_WAIT, |deadline| {
                    deadline.saturating_duration_since(now)
                });
                let mut produced = match inbox.recv_timeout(wait) {
                    Ok(Command::Stop) | Err(RecvTimeoutError::Disconnected) => return,
                    Ok(command) => engine.handle(command, Instant::now()),
                    Err(RecvTimeoutError::Timeout) => Vec::new(),
                };
                // Here and not on the timeout: a timeout never fires while the inbox
                // has work, and a pane's recovery must not wait for a flood to end.
                let now = Instant::now();
                if engine
                    .next_deadline()
                    .is_some_and(|deadline| deadline <= now)
                {
                    produced.extend(engine.poll(now));
                }
                for output in produced {
                    if outputs.send(output).is_err() {
                        return;
                    }
                }
            }
        })
        .expect("spawning the terminal engine thread");
    EngineHandle {
        commands,
        thread: Some(thread),
    }
}

#[cfg(test)]
#[path = "engine_tests.rs"]
mod tests;
