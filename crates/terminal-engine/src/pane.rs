//! One mounted pane: the handshake and the event consumer of `TerminalPane.tsx`,
//! without the view.
//!
//! Every terminal write is synchronous here, so the callbacks the desktop hangs
//! on xterm's write completion run in line, and the renderer's refusal paths
//! (queue overflow, a tail that cannot be queued) do not exist.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::deferred::{DeferredOutput, DeferredOutputAdmission, DeferredTerminalOutputQueue};
use crate::event::{GridSize, PaneEvent};
use crate::hub::{HubOutput, PaneHealth, TerminalEventHub, VisibilityCheckpoint};
use crate::pager::{CachedHistoryState, HistoryPageTrigger, PaneHistoryPager};
use crate::reading::{
    TerminalReadingState, reading_may_page_history, reading_screen_is_current,
    reading_state_for_authoritative_screen, reading_state_for_output, reading_state_for_viewport,
    return_to_live,
};
use crate::reveal::{PaneRevealEffect, PaneRevealState, output_after_recovery, reduce_pane_reveal};
use crate::reveal_retry::{
    REVEAL_RETRY_DELAY_MS, RevealFailureAction, is_quiet_reveal_error, reveal_failure_action,
};
use crate::terminal::{GridOutcome, PaneTerminal, PaneWriter};
use crate::watchdog::{PaneDegradedReason, PaneDegradedWatchdog};

const REVEAL_VOID_TIMEOUT: Duration = Duration::from_millis(4_000);
const REVEAL_VOID_MAX_ATTEMPTS: u32 = 3;
const PANE_HANDOFF_TIMEOUT: Duration = Duration::from_millis(2_000);

#[derive(Debug)]
pub enum EngineOutput {
    /// The terminal the UI paints for this mount. Painted once on this
    /// output; after that [`EngineOutput::Dirty`] says when to paint again.
    Mounted {
        pane_id: String,
        terminal: Arc<PaneTerminal>,
    },
    RequestSeed {
        client_id: String,
        pane_id: String,
        reason: String,
    },
    RequestHistory {
        client_id: String,
        pane_id: String,
        lines: u32,
        skip: usize,
        serial: u64,
    },
    /// Answer with [`crate::Command::VisibilityAnswered`] and this token.
    SetVisibility {
        client_id: String,
        pane_id: String,
        visible: bool,
        holds_snapshot: bool,
        checkpoint: VisibilityCheckpoint,
        token: u64,
    },
    /// The terminal's dirty flag went up. It stays up until `take_dirty`.
    Dirty {
        pane_id: String,
    },
    /// The pane has content to show. Once per mount.
    Revealed {
        pane_id: String,
    },
    GridApplied {
        pane_id: String,
        size: GridSize,
    },
    ReadingChanged {
        pane_id: String,
        state: TerminalReadingState,
    },
    Diagnostic {
        pane_id: String,
        message: String,
    },
    RendererDiagnostic {
        pane_id: String,
        message: Option<String>,
    },
    SeedDiagnostic {
        pane_id: String,
        message: Option<String>,
    },
    Incident {
        kind: &'static str,
        detail: Value,
    },
    Repaint {
        pane_id: String,
        sequence: u64,
        generation: u64,
    },
    Epoch(u64),
    /// The host gave up resuming this pane's stream; reconnect the link. Names
    /// the epoch it was admitted under, so a stall an earlier bridge left in the
    /// queue is not taken for this one.
    FlowStalled {
        pane_id: String,
        epoch: Option<u64>,
    },
    /// Acknowledge this frame to the host. Emitted after it has been applied.
    FrameConsumed {
        byte_length: usize,
        epoch: Option<u64>,
    },
}

pub(crate) struct PendingHandoff {
    pub(crate) token: u64,
    pub(crate) client_id: String,
    pub(crate) terminal_epoch: u64,
    /// A reveal stopped waiting. A refusal arriving later still costs a seed.
    pub(crate) timed_out: bool,
}

/// What every pane needs from its engine.
pub(crate) struct Shared {
    pub(crate) hub: TerminalEventHub,
    pub(crate) hub_out: Vec<HubOutput>,
    pub(crate) out: Vec<EngineOutput>,
    pub(crate) client_id: Option<String>,
    /// A hide the host has not answered. A reveal of the same pane waits for it,
    /// so a late hide cannot overtake the reveal at the host.
    pub(crate) handoffs: HashMap<String, PendingHandoff>,
    pub(crate) next_token: u64,
}

impl Shared {
    pub(crate) fn token(&mut self) -> u64 {
        self.next_token += 1;
        self.next_token
    }

    pub(crate) fn request_seed(&mut self, pane_id: &str, reason: &str) {
        if let Some(client_id) = self.client_id.clone() {
            self.out.push(EngineOutput::RequestSeed {
                client_id,
                pane_id: pane_id.to_owned(),
                reason: reason.to_owned(),
            });
        }
    }
}

fn journal(out: &mut Vec<EngineOutput>) -> impl FnMut(&'static str, Value) + '_ {
    |kind, detail| out.push(EngineOutput::Incident { kind, detail })
}

fn truncated(error: &str) -> String {
    error.chars().take(200).collect()
}

/// The handoff screen a pane is holding, and the checkpoint it was hidden at.
#[derive(Clone, Copy)]
pub(crate) struct Held {
    pub(crate) terminal_epoch: u64,
    pub(crate) output_generation: u64,
}

pub(crate) struct HideHandoff {
    pub(crate) renderer_epoch: Option<u64>,
    pub(crate) output_generation: u64,
    pub(crate) screen_is_current: bool,
    pub(crate) history: CachedHistoryState,
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct ScreenOnDisplay {
    generation: u64,
    terminal_epoch: Option<u64>,
}

enum RevealPhase {
    Idle,
    WaitingForHandoff {
        deadline: Instant,
        checkpoint: VisibilityCheckpoint,
        retries: u32,
    },
    AwaitingAnswer {
        token: u64,
        content_before: u64,
        checkpoint: VisibilityCheckpoint,
        retries: u32,
    },
    RetryAt {
        deadline: Instant,
        checkpoint: VisibilityCheckpoint,
        retries: u32,
    },
}

pub(crate) struct Pane {
    id: String,
    pub(crate) writer: PaneWriter,
    reveal: PaneRevealState,
    deferred: DeferredTerminalOutputQueue,
    watchdog: PaneDegradedWatchdog,
    reading: TerminalReadingState,
    pager: PaneHistoryPager,
    applied_generation: u64,
    renderer_epoch: Option<u64>,
    /// Exactly one retained screen with nothing written after it. What a
    /// verified resume is checked against: a tail on the wrong screen is a hole
    /// nothing later repairs.
    screen_on_display: Option<ScreenOnDisplay>,
    pub(crate) held: Option<Held>,
    content_arrivals: u64,
    revealed: bool,
    reveal_key: Option<(String, u64, u64)>,
    reveal_attempt: u64,
    phase: RevealPhase,
    void_watch: Option<(Instant, u32)>,
    seed_diagnostic_for_next_seed: bool,
    stream_grid: Option<GridSize>,
}

impl Pane {
    pub(crate) fn new(id: &str, writer: PaneWriter) -> Self {
        Self {
            id: id.to_owned(),
            writer,
            reveal: PaneRevealState::default(),
            deferred: DeferredTerminalOutputQueue::default(),
            watchdog: PaneDegradedWatchdog::default(),
            reading: TerminalReadingState::Live,
            pager: PaneHistoryPager::new(id),
            applied_generation: 0,
            renderer_epoch: None,
            screen_on_display: None,
            held: None,
            content_arrivals: 0,
            revealed: false,
            reveal_key: None,
            reveal_attempt: 0,
            phase: RevealPhase::Idle,
            void_watch: None,
            seed_diagnostic_for_next_seed: false,
            stream_grid: None,
        }
    }

    /// Puts a retained screen back on display.
    pub(crate) fn restored(
        id: &str,
        writer: PaneWriter,
        held: Held,
        history: CachedHistoryState,
        shared: &mut Shared,
    ) -> Self {
        // `Mounted` is a full paint, so a flag left up while hidden must not eat
        // the next wake.
        writer.terminal().take_dirty();
        let mut pane = Self::new(id, writer);
        pane.reveal.has_local_state = true;
        pane.held = Some(held);
        pane.screen_on_display = Some(ScreenOnDisplay {
            generation: held.output_generation,
            terminal_epoch: Some(held.terminal_epoch),
        });
        pane.pager.restore(history);
        pane.applied_generation = held.output_generation;
        pane.publish_initial_paint(held.output_generation, Some(held.terminal_epoch), shared);
        // The grid kept its position, so the viewport is already wherever the
        // user left it. No viewport event will say so.
        let at_bottom = pane.writer.terminal().lock().grid().display_offset() == 0;
        pane.reading = reading_state_for_viewport(pane.reading, at_bottom);
        pane
    }

    pub(crate) fn next_deadline(&self) -> Option<Instant> {
        let phase = match self.phase {
            RevealPhase::WaitingForHandoff { deadline, .. }
            | RevealPhase::RetryAt { deadline, .. } => Some(deadline),
            _ => None,
        };
        [
            phase,
            self.watchdog.deadline(),
            self.void_watch.map(|(deadline, _)| deadline),
            self.writer.sync_deadline(),
        ]
        .into_iter()
        .flatten()
        .min()
    }

    pub(crate) fn poll(&mut self, shared: &mut Shared, now: Instant) {
        if self
            .writer
            .sync_deadline()
            .is_some_and(|deadline| deadline <= now)
        {
            self.writer.flush_sync();
        }
        if let Some(retry) = self.watchdog.poll(now) {
            let reason = retry.reason;
            shared.out.push(EngineOutput::Incident {
                kind: "pane.degraded",
                detail: json!({ "paneId": self.id, "reason": reason.name(), "attempt": retry.attempt }),
            });
            if retry.attempt == 0 {
                self.renderer_diagnostic(
                    Some(format!("{}; retrying…", reason.description())),
                    shared,
                );
            }
            shared.hub.retry_pane_seed(&self.id, &mut shared.hub_out);
            shared.request_seed(
                &self.id,
                &format!(
                    "Pane {} stayed degraded ({}) past its recovery bound",
                    self.id,
                    reason.name()
                ),
            );
            // A seed cannot help a pane the host believes is hidden.
            self.reveal_attempt += 1;
            self.reveal_for_current_epoch(shared, now);
        }
        match self.phase {
            RevealPhase::WaitingForHandoff {
                deadline,
                checkpoint,
                retries,
            } if deadline <= now => {
                if let Some(handoff) = shared.handoffs.get_mut(&self.id) {
                    handoff.timed_out = true;
                }
                shared.out.push(EngineOutput::Incident {
                    kind: "terminal.pane.handoffTimeouts",
                    detail: json!({ "paneId": self.id }),
                });
                self.send_reveal(checkpoint, retries, shared);
            }
            RevealPhase::RetryAt {
                deadline,
                checkpoint,
                retries,
            } if deadline <= now => self.run_reveal(checkpoint, retries, shared, now),
            _ => {}
        }
        if let Some((deadline, attempt)) = self.void_watch
            && deadline <= now
        {
            self.void_watch = None;
            // A degraded episode already has a bounded recovery running.
            if !self.watchdog.degraded() {
                shared.out.push(EngineOutput::Incident {
                    kind: "pane.revealVoid",
                    detail: json!({
                        "paneId": self.id,
                        "msWaited": REVEAL_VOID_TIMEOUT.as_millis() as u64,
                        "attempt": attempt,
                    }),
                });
                shared.request_seed(
                    &self.id,
                    &format!("Pane {} produced nothing after its reveal", self.id),
                );
                if attempt + 1 < REVEAL_VOID_MAX_ATTEMPTS {
                    self.void_watch = Some((now + REVEAL_VOID_TIMEOUT, attempt + 1));
                }
            }
        }
        self.wake_ui(shared);
    }

    pub(crate) fn reveal_for_current_epoch(&mut self, shared: &mut Shared, now: Instant) {
        let Some(client_id) = shared.client_id.clone() else {
            return;
        };
        let Some(checkpoint) = shared.hub.visibility_checkpoint(&self.id) else {
            return;
        };
        let key = (client_id, checkpoint.terminal_epoch, self.reveal_attempt);
        if self.reveal_key.as_ref() == Some(&key) {
            return;
        }
        self.reveal_key = Some(key);
        self.phase = RevealPhase::Idle;
        self.void_watch = None;
        self.run_reveal(checkpoint, 0, shared, now);
    }

    fn run_reveal(
        &mut self,
        checkpoint: VisibilityCheckpoint,
        retries: u32,
        shared: &mut Shared,
        now: Instant,
    ) {
        if shared
            .handoffs
            .get(&self.id)
            .is_some_and(|handoff| !handoff.timed_out)
        {
            self.phase = RevealPhase::WaitingForHandoff {
                deadline: now + PANE_HANDOFF_TIMEOUT,
                checkpoint,
                retries,
            };
            return;
        }
        self.send_reveal(checkpoint, retries, shared);
    }

    fn send_reveal(&mut self, checkpoint: VisibilityCheckpoint, retries: u32, shared: &mut Shared) {
        self.phase = RevealPhase::Idle;
        let Some(client_id) = shared.client_id.clone() else {
            return;
        };
        let Some(current) = shared.hub.visibility_checkpoint(&self.id) else {
            return;
        };
        if current.terminal_epoch != checkpoint.terminal_epoch {
            return;
        }
        let renderer_matches = self.renderer_epoch == Some(current.terminal_epoch);
        let held = self
            .held
            .filter(|held| held.terminal_epoch == current.terminal_epoch);
        self.reveal = PaneRevealState {
            ready: false,
            has_local_state: renderer_matches || held.is_some(),
        };
        // A pane holding the handoff screen names that screen's cutoff: it is
        // the one the host recorded, and the tail it answers with continues it.
        let output_generation = match held {
            Some(held) => held.output_generation,
            None if renderer_matches => current.output_generation,
            None => 0,
        };
        let token = shared.token();
        self.phase = RevealPhase::AwaitingAnswer {
            token,
            content_before: self.content_arrivals,
            checkpoint,
            retries,
        };
        shared.out.push(EngineOutput::SetVisibility {
            client_id,
            pane_id: self.id.clone(),
            visible: true,
            holds_snapshot: held.is_some(),
            checkpoint: VisibilityCheckpoint {
                terminal_epoch: current.terminal_epoch,
                output_generation,
            },
            token,
        });
    }

    /// True when `token` was this pane's reveal.
    pub(crate) fn reveal_answered(
        &mut self,
        token: u64,
        result: &Result<(), String>,
        shared: &mut Shared,
        now: Instant,
    ) -> bool {
        let RevealPhase::AwaitingAnswer {
            token: awaited,
            content_before,
            checkpoint,
            retries,
        } = self.phase
        else {
            return false;
        };
        if awaited != token {
            return false;
        }
        self.phase = RevealPhase::Idle;
        match result {
            Ok(()) => {
                self.watchdog.clear(PaneDegradedReason::RevealFailed);
                // The host took the request; nothing yet proves it acted on it.
                if self.content_arrivals == content_before {
                    self.void_watch = Some((now + REVEAL_VOID_TIMEOUT, 0));
                }
            }
            Err(error) => self.reveal_failed(error, checkpoint, retries, shared, now),
        }
        true
    }

    fn reveal_failed(
        &mut self,
        error: &str,
        checkpoint: VisibilityCheckpoint,
        retries: u32,
        shared: &mut Shared,
        now: Instant,
    ) {
        match reveal_failure_action(error, true, retries) {
            RevealFailureAction::Ignore => {}
            RevealFailureAction::Rebuild => {
                // The host refused the checkpoint, and nothing here can build a
                // newer one until the epoch frame arrives. A seed carries none.
                self.reveal_key = None;
                self.held = None;
                shared.out.push(EngineOutput::Incident {
                    kind: "pane.revealRebuilt",
                    detail: json!({
                        "paneId": self.id,
                        "hubEpoch": shared.hub.generation_epoch(),
                        "error": truncated(error),
                    }),
                });
                self.watchdog.note(PaneDegradedReason::RevealFailed, now);
                shared.request_seed(
                    &self.id,
                    &format!(
                        "Could not reseed {} after a stale visibility epoch",
                        self.id
                    ),
                );
            }
            RevealFailureAction::Retry => {
                if retries == 0 {
                    shared.out.push(EngineOutput::Incident {
                        kind: "pane.revealRetry",
                        detail: json!({
                            "paneId": self.id,
                            "attempt": self.reveal_attempt,
                            "error": truncated(error),
                        }),
                    });
                }
                self.phase = RevealPhase::RetryAt {
                    deadline: now + Duration::from_millis(REVEAL_RETRY_DELAY_MS),
                    checkpoint,
                    retries: retries + 1,
                };
            }
            RevealFailureAction::Degrade => {
                self.reveal_key = None;
                self.watchdog.note(PaneDegradedReason::RevealFailed, now);
                shared.out.push(EngineOutput::Incident {
                    kind: "pane.revealFailed",
                    detail: json!({
                        "paneId": self.id,
                        "retriesUsed": retries,
                        "error": truncated(error),
                    }),
                });
                if !is_quiet_reveal_error(error) {
                    shared.out.push(EngineOutput::Diagnostic {
                        pane_id: self.id.clone(),
                        message: format!("Could not mark {} visible: {error}", self.id),
                    });
                }
                if shared.hub.generation_epoch() == Some(checkpoint.terminal_epoch) {
                    shared.request_seed(
                        &self.id,
                        &format!("Could not reseed {} after a visibility conflict", self.id),
                    );
                }
            }
        }
    }

    pub(crate) fn handoff_finished(&mut self, shared: &mut Shared) {
        if let RevealPhase::WaitingForHandoff {
            checkpoint,
            retries,
            ..
        } = self.phase
        {
            self.send_reveal(checkpoint, retries, shared);
        }
    }

    pub(crate) fn client_changed(&mut self) {
        self.phase = RevealPhase::Idle;
        self.void_watch = None;
    }

    pub(crate) fn epoch_changed(&mut self, shared: &mut Shared, now: Instant) {
        self.held = None;
        self.deferred.reset();
        self.stream_grid = None;
        self.renderer_epoch = None;
        self.reveal = PaneRevealState::default();
        self.void_watch = None;
        self.reveal_for_current_epoch(shared, now);
    }

    pub(crate) fn begin_hide(&mut self) -> HideHandoff {
        self.watchdog.stop();
        self.void_watch = None;
        self.phase = RevealPhase::Idle;
        self.writer.flush_sync();
        HideHandoff {
            renderer_epoch: self.renderer_epoch,
            output_generation: self.applied_generation,
            screen_is_current: reading_screen_is_current(self.reading),
            history: self.pager.snapshot(),
        }
    }

    pub(crate) fn health_changed(&mut self, health: PaneHealth, now: Instant) {
        // While `awaiting_seed` stands the hub drops this pane's events, so this
        // is the only way the pane can find out it has been silenced.
        let mut mirror = |reason, active| {
            if active {
                self.watchdog.note(reason, now);
            } else {
                self.watchdog.clear(reason);
            }
        };
        mirror(PaneDegradedReason::HubAwaitingSeed, health.awaiting_seed);
        mirror(
            PaneDegradedReason::HubConflictReseed,
            health.conflict_reseed_requested,
        );
    }

    /// This occupies tmux's resize boundary in the output stream.
    pub(crate) fn stream_grid(&mut self, size: Option<GridSize>, shared: &mut Shared) {
        self.stream_grid = size;
        if let Some(size) = size {
            self.set_grid(size, shared);
        }
    }

    /// A size from the UI. The stream's own grid wins: later snapshots position the
    /// pane but must not move its parser off tmux's resize boundary.
    pub(crate) fn ui_grid(&mut self, size: GridSize, shared: &mut Shared) {
        self.set_grid(self.stream_grid.unwrap_or(size), shared);
    }

    pub(crate) fn set_grid(&mut self, size: GridSize, shared: &mut Shared) {
        match self.writer.set_grid(size) {
            GridOutcome::Applied(size) => {
                // A reflow moves rows across tmux's history boundary.
                self.pager.note_grid_changed();
                shared.out.push(EngineOutput::GridApplied {
                    pane_id: self.id.clone(),
                    size,
                });
                self.wake_ui(shared);
            }
            GridOutcome::Unchanged => {}
            GridOutcome::Rejected(message) => shared.out.push(EngineOutput::Diagnostic {
                pane_id: self.id.clone(),
                message,
            }),
        }
    }

    pub(crate) fn request_page(&mut self, trigger: HistoryPageTrigger, shared: &mut Shared) {
        if trigger == HistoryPageTrigger::ScrolledToTop && !reading_may_page_history(self.reading) {
            return;
        }
        self.ask_for_page(trigger, shared);
    }

    fn ask_for_page(&mut self, trigger: HistoryPageTrigger, shared: &mut Shared) {
        let Some(client_id) = shared.client_id.clone() else {
            return;
        };
        let facts = self.writer.facts();
        let request = self
            .pager
            .request_page(trigger, facts, &mut journal(&mut shared.out));
        if let Some(request) = request {
            shared.out.push(EngineOutput::RequestHistory {
                client_id,
                pane_id: self.id.clone(),
                lines: request.lines,
                skip: request.skip,
                serial: request.serial,
            });
        }
    }

    pub(crate) fn history_request_failed(&mut self, serial: u64, error: &str, shared: &mut Shared) {
        self.pager
            .request_failed(serial, error, &mut journal(&mut shared.out));
    }

    pub(crate) fn viewport_changed(&mut self, at_bottom: bool, shared: &mut Shared, now: Instant) {
        self.set_reading(reading_state_for_viewport(self.reading, at_bottom), shared);
        if self.reading == TerminalReadingState::Reading && self.deferred.byte_length() > 0 {
            // Output waiting behind a reveal becomes skipped output at this
            // moment, not something a later answer may flush into this view.
            self.skip_output_for_historical_screen(shared);
            self.deferred.reset();
        }
        if at_bottom
            && matches!(
                self.reading,
                TerminalReadingState::Reading | TerminalReadingState::Outdated
            )
        {
            self.resume_live(
                "Returning to the bottom requires a current terminal screen",
                shared,
                now,
            );
        }
    }

    pub(crate) fn resume_live(&mut self, reason: &str, shared: &mut Shared, now: Instant) {
        let decision = return_to_live(self.reading);
        self.set_reading(decision.state, shared);
        if decision.scroll_locally {
            self.writer.scroll_to_bottom();
            self.wake_ui(shared);
            return;
        }
        if !decision.request_seed {
            return;
        }
        // The historical screen stays up while the replacement is on the wire.
        self.reveal = PaneRevealState::default();
        self.deferred.reset();
        self.held = None;
        self.watchdog.note(PaneDegradedReason::RendererReseed, now);
        shared.request_seed(&self.id, reason);
    }

    fn set_reading(&mut self, state: TerminalReadingState, shared: &mut Shared) {
        if self.reading != state {
            self.reading = state;
            shared.out.push(EngineOutput::ReadingChanged {
                pane_id: self.id.clone(),
                state,
            });
        }
    }

    fn wake_ui(&mut self, shared: &mut Shared) {
        if self.writer.take_wake() {
            shared.out.push(EngineOutput::Dirty {
                pane_id: self.id.clone(),
            });
        }
    }

    fn renderer_diagnostic(&self, message: Option<String>, shared: &mut Shared) {
        shared.out.push(EngineOutput::RendererDiagnostic {
            pane_id: self.id.clone(),
            message,
        });
    }

    fn seed_diagnostic(&self, message: Option<String>, shared: &mut Shared) {
        shared.out.push(EngineOutput::SeedDiagnostic {
            pane_id: self.id.clone(),
            message,
        });
    }

    fn reveal_terminal(&mut self, shared: &mut Shared) {
        if !self.revealed {
            self.revealed = true;
            shared.out.push(EngineOutput::Revealed {
                pane_id: self.id.clone(),
            });
        }
    }

    fn commit_rendered(
        &mut self,
        generation: u64,
        terminal_epoch: Option<u64>,
        shared: &mut Shared,
    ) {
        shared
            .hub
            .mark_rendered(&self.id, generation, terminal_epoch, &mut shared.hub_out);
    }

    /// Content that establishes this screen as belonging to `terminal_epoch`.
    fn publish_initial_paint(
        &mut self,
        generation: u64,
        terminal_epoch: Option<u64>,
        shared: &mut Shared,
    ) {
        self.reveal_terminal(shared);
        if shared.hub.generation_epoch() == terminal_epoch {
            self.renderer_epoch = terminal_epoch;
        }
        self.commit_rendered(generation, terminal_epoch, shared);
    }

    fn skip_output_for_historical_screen(&mut self, shared: &mut Shared) -> bool {
        if !matches!(
            self.reading,
            TerminalReadingState::Reading | TerminalReadingState::Outdated
        ) {
            return false;
        }
        let decision = reading_state_for_output(self.reading);
        self.set_reading(decision.state, shared);
        if decision.became_outdated {
            // A page on the wire was captured against tmux's newer live display,
            // not the frozen screen it would rewrite.
            self.pager.note_screen_gone();
        }
        true
    }

    fn note_applied(&mut self, generation: u64) {
        self.applied_generation = self.applied_generation.max(generation);
    }

    fn write_output(&mut self, data: &[u8], generation: u64) {
        self.writer.write(data);
        self.note_applied(generation);
    }

    /// A blank terminal and nothing above it, while a seed is owed.
    fn blank(&mut self) {
        self.writer.seed(&[]);
        self.applied_generation = 0;
    }

    fn flush_deferred_output(&mut self, after_generation: Option<u64>, shared: &mut Shared) {
        let mut deferred = self.deferred.drain();
        if let Some(through) = after_generation {
            deferred =
                output_after_recovery(deferred, through, |item: &DeferredOutput| item.generation);
        }
        if deferred.iter().any(|item| !item.data.is_empty())
            && self.skip_output_for_historical_screen(shared)
        {
            return;
        }
        if !deferred.is_empty() {
            self.screen_on_display = None;
        }
        for item in deferred {
            self.write_output(&item.data, item.generation);
            self.reveal_terminal(shared);
            self.commit_rendered(item.generation, item.terminal_epoch, shared);
        }
    }

    pub(crate) fn apply(&mut self, event: PaneEvent, shared: &mut Shared, now: Instant) {
        self.apply_event(event, shared, now);
        self.wake_ui(shared);
    }

    fn apply_event(&mut self, event: PaneEvent, shared: &mut Shared, now: Instant) {
        if let PaneEvent::History {
            data, history_size, ..
        } = &event
        {
            self.receive_history(data, *history_size, shared);
            return;
        }
        if self.reading == TerminalReadingState::Refreshing
            && matches!(event, PaneEvent::Resource(_))
        {
            // Returning live superseded the reveal this answers, and the one
            // seed already requested owns recovery now.
            return;
        }
        let generation = match &event {
            PaneEvent::Seed { generation, .. } | PaneEvent::Output { generation, .. } => {
                *generation
            }
            PaneEvent::Resource(resource) => resource.generation,
            _ => 0,
        };
        let (state, effect) = reduce_pane_reveal(self.reveal, event);
        self.reveal = state;
        let event_epoch = shared.hub.generation_epoch();
        if matches!(
            effect,
            PaneRevealEffect::Seed(_)
                | PaneRevealEffect::Output(_)
                | PaneRevealEffect::Resume { .. }
        ) {
            self.content_arrivals += 1;
            self.void_watch = None;
        }
        match effect {
            PaneRevealEffect::Seed(data) => self.apply_seed(&data, generation, event_epoch, shared),
            PaneRevealEffect::Output(data) => {
                self.watchdog.note_healthy();
                if self.skip_output_for_historical_screen(shared) {
                    return;
                }
                self.screen_on_display = None;
                self.write_output(&data, generation);
                self.reveal_terminal(shared);
                self.commit_rendered(generation, event_epoch, shared);
            }
            PaneRevealEffect::DeferOutput(data) => {
                if self.skip_output_for_historical_screen(shared) {
                    return;
                }
                let admission = self.deferred.enqueue(DeferredOutput {
                    data,
                    generation,
                    terminal_epoch: event_epoch,
                });
                if admission == DeferredOutputAdmission::Overflow {
                    self.reveal = PaneRevealState::default();
                    self.watchdog
                        .note(PaneDegradedReason::DeferredOverflow, now);
                    shared.request_seed(
                        &self.id,
                        "Output arrived before pane recovery exceeded its byte or record budget",
                    );
                }
            }
            PaneRevealEffect::AwaitSeed {
                reason,
                request_seed,
                unusable_state,
            } => {
                self.drop_screen(shared);
                if let Some(state) = unusable_state {
                    shared.out.push(EngineOutput::Incident {
                        kind: "pane.revealDeadEnd",
                        detail: json!({ "paneId": self.id, "state": state.name() }),
                    });
                }
                self.watchdog
                    .note(PaneDegradedReason::PaneAwaitingSeed, now);
                self.blank();
                if !reason.is_empty() {
                    self.renderer_diagnostic(
                        Some(format!("{reason}; waiting for a fresh terminal seed…")),
                        shared,
                    );
                }
                if request_seed {
                    shared.request_seed(&self.id, &reason);
                }
            }
            PaneRevealEffect::Resume {
                raw_tail,
                snapshot_generation,
                tail_through_generation,
            } => self.apply_resume(
                &raw_tail,
                snapshot_generation,
                tail_through_generation,
                event_epoch,
                shared,
                now,
            ),
            PaneRevealEffect::HideEchoAfterReady => shared.out.push(EngineOutput::Incident {
                kind: "pane.hideEchoAfterReady",
                detail: json!({ "paneId": self.id }),
            }),
            PaneRevealEffect::AwaitAnswer { showing_screen } => {
                if !showing_screen {
                    self.watchdog
                        .note(PaneDegradedReason::PaneAwaitingSeed, now);
                    shared.request_seed(
                        &self.id,
                        &format!(
                            "Pane {} has nothing to show while its reveal goes unanswered",
                            self.id
                        ),
                    );
                }
            }
            PaneRevealEffect::Diagnostic(message) => {
                self.seed_diagnostic_for_next_seed = true;
                self.seed_diagnostic(Some(message), shared);
            }
            PaneRevealEffect::None => {
                if self.reveal.ready {
                    self.flush_deferred_output(None, shared);
                }
            }
        }
    }

    /// The bookkeeping common to the arms where the screen is about to be blank.
    fn drop_screen(&mut self, shared: &mut Shared) {
        self.set_reading(reading_state_for_authoritative_screen(), shared);
        self.held = None;
        self.deferred.reset();
        self.screen_on_display = None;
        self.pager.note_screen_gone();
    }

    fn apply_seed(
        &mut self,
        data: &[u8],
        generation: u64,
        event_epoch: Option<u64>,
        shared: &mut Shared,
    ) {
        self.set_reading(reading_state_for_authoritative_screen(), shared);
        self.held = None;
        self.deferred.reset();
        self.pager.note_screen_seeded();
        self.screen_on_display = None;
        self.watchdog.note_healthy();
        self.writer.seed(data);
        self.applied_generation = generation;
        self.publish_initial_paint(generation, event_epoch, shared);
        // Only from a seed: a restored or resumed screen already carries
        // whatever history it had.
        self.ask_for_page(HistoryPageTrigger::Prefetch, shared);
        self.renderer_diagnostic(None, shared);
        if self.seed_diagnostic_for_next_seed {
            self.seed_diagnostic_for_next_seed = false;
        } else {
            self.seed_diagnostic(None, shared);
        }
    }

    fn apply_resume(
        &mut self,
        raw_tail: &[u8],
        snapshot_generation: u64,
        tail_through_generation: u64,
        event_epoch: Option<u64>,
        shared: &mut Shared,
        now: Instant,
    ) {
        // The host verified its record of the handoff. Only this side can verify
        // that the screen here is the one that record names.
        let holding_the_verified_screen = self.screen_on_display
            == Some(ScreenOnDisplay {
                generation: snapshot_generation,
                terminal_epoch: event_epoch,
            });
        if !holding_the_verified_screen {
            self.reveal = PaneRevealState::default();
            self.drop_screen(shared);
            self.watchdog
                .note(PaneDegradedReason::PaneAwaitingSeed, now);
            self.blank();
            let reason = format!(
                "Pane {} is not showing the screen the host resumed it from",
                self.id
            );
            self.renderer_diagnostic(
                Some(format!("{reason}; waiting for a fresh terminal seed…")),
                shared,
            );
            shared.request_seed(&self.id, &reason);
            return;
        }
        let has_output = !raw_tail.is_empty() || self.deferred.byte_length() > 0;
        if has_output && self.skip_output_for_historical_screen(shared) {
            // The kept screen is still the exact view being read. The tail is
            // covered by the one seed requested when the user returns live.
            self.deferred.reset();
            self.watchdog.note_healthy();
            self.renderer_diagnostic(None, shared);
            return;
        }
        self.write_output(raw_tail, tail_through_generation);
        self.publish_initial_paint(tail_through_generation, event_epoch, shared);
        if !raw_tail.is_empty() {
            self.screen_on_display = None;
        }
        self.flush_deferred_output(Some(tail_through_generation), shared);
        self.watchdog.note_healthy();
        self.renderer_diagnostic(None, shared);
    }

    fn receive_history(&mut self, data: &[u8], history_size: Option<u32>, shared: &mut Shared) {
        let writer = &mut self.writer;
        self.pager.receive(
            data,
            history_size,
            |page, anchor| writer.prepend_history(page, anchor),
            &mut journal(&mut shared.out),
        );
    }
}
