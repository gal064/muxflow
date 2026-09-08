use super::*;
use crate::service::terminal::OutputCharge;
use crate::service::terminal::degradation::emit_pane_degradations;
use crate::service::topology_output_trigger::TopologyOutputTrigger;

pub(in crate::service::terminal) fn with_active_resources<T>(
    resources: &Arc<Mutex<PaneResourceStore>>,
    stopped: &AtomicBool,
    update: impl FnOnce(&mut PaneResourceStore) -> T,
) -> Option<T> {
    let mut resources = resources.lock().unwrap();
    if stopped.load(Ordering::Acquire) {
        return None;
    }
    Some(update(&mut resources))
}

pub(super) fn emit_resnapshot(
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

/// Queues one terminal record, charging the delivery window for it.
///
/// Returns whether the record was admitted, which is the caller's debt to the
/// window: after releasing the emission fence, a control reader that admitted
/// must [`OutputCredit::await_window`] before it emits again. Admission itself
/// never waits — see the `output_credit` module docs.
#[allow(clippy::too_many_arguments)]
pub(super) fn emit_terminal(
    sender: &mpsc::Sender<SequencerControl>,
    overflowed: &AtomicBool,
    kind: v1::EventKind,
    pane_id: String,
    data: Vec<u8>,
    generation: u64,
    stopped: &AtomicBool,
    output_credit: &OutputCredit,
) -> bool {
    emit_terminal_bytes(
        sender,
        overflowed,
        kind,
        v1::TerminalBytes {
            pane_id,
            data,
            generation,
            ..Default::default()
        },
        stopped,
        output_credit,
    )
}

/// Queues one pane's scrollback answer.
///
/// Its own entry point because it is the only terminal record that carries a
/// number the output stream has no use for: how much history tmux holds, which
/// is what lets the renderer stop paging at the top instead of asking forever.
/// `history_size` is `None` when the probe did not answer — a pane that went
/// away between the capture and it — and the renderer reads that as a page to
/// ask for again, never as the end of the history.
pub(super) fn emit_terminal_history(
    sender: &mpsc::Sender<SequencerControl>,
    overflowed: &AtomicBool,
    pane_id: String,
    data: Vec<u8>,
    history_size: Option<u32>,
    stopped: &AtomicBool,
    output_credit: &OutputCredit,
) -> bool {
    emit_terminal_bytes(
        sender,
        overflowed,
        v1::EventKind::TerminalHistory,
        v1::TerminalBytes {
            pane_id,
            data,
            // Deliberately not part of the output ordering: see the block that
            // produces it.
            generation: 0,
            history_size: history_size.unwrap_or(0),
            history_size_known: history_size.is_some(),
        },
        stopped,
        output_credit,
    )
}

fn emit_terminal_bytes(
    sender: &mpsc::Sender<SequencerControl>,
    overflowed: &AtomicBool,
    kind: v1::EventKind,
    terminal: v1::TerminalBytes,
    stopped: &AtomicBool,
    output_credit: &OutputCredit,
) -> bool {
    // Terminal bytes are lossless and already arrive on the dedicated control
    // reader thread. Let the bounded sequencer queue propagate socket pressure
    // back to that reader; tmux can then apply its own pause/continue protocol.
    // A nonblocking send here turned a normal 100 ms / 100 Mbit bandwidth-delay
    // window into a full-connection resync as soon as 1,024 records accumulated.
    let charge = OutputCharge::terminal(terminal.data.len());
    let Ok(reservation) = output_credit.admit(charge, stopped) else {
        // A stopped attachment is a deliberate local teardown; only a closed
        // credit is a connection-wide loss worth the overflow resync.
        if !stopped.load(Ordering::Acquire) {
            overflowed.store(true, Ordering::Release);
        }
        return false;
    };
    if sender
        .blocking_send(SequencerControl::OrderedEvent(v1::HostEvent {
            kind: kind.into(),
            terminal: Some(terminal),
            terminal_delivery_bytes: charge.bytes,
            terminal_delivery_records: charge.records,
            ..Default::default()
        }))
        .is_err()
    {
        overflowed.store(true, Ordering::Release);
        false
    } else {
        reservation.commit();
        true
    }
}

pub(in crate::service::terminal) struct OutputEmission<'a> {
    pub(in crate::service::terminal) connection_epoch: crate::diagnostics::PerfConnectionEpoch,
    pub(in crate::service::terminal) sender: &'a mpsc::Sender<SequencerControl>,
    pub(in crate::service::terminal) overflowed: &'a AtomicBool,
    pub(in crate::service::terminal) resources: &'a Arc<Mutex<PaneResourceStore>>,
    pub(in crate::service::terminal) terminal_generation: &'a AtomicU64,
    pub(in crate::service::terminal) stopped: &'a AtomicBool,
    pub(in crate::service::terminal) output_credit: &'a OutputCredit,
    pub(in crate::service::terminal) emission_order: &'a Mutex<()>,
    pub(in crate::service::terminal) topology_trigger: &'a TopologyOutputTrigger,
    /// When the control-stream read that produced this batch returned; the
    /// start of the output leg this emission ends.
    pub(in crate::service::terminal) read_started: std::time::Instant,
}

impl OutputEmission<'_> {
    pub(in crate::service::terminal) fn record(self, pane_id: String, data: Vec<u8>) {
        // Before the fence, and never under it. tmux stays silent for a
        // title-driven window rename or a pane's cwd moving, but both always
        // come with output; a debounced dirty mark here is what gets them to
        // the desktop without waiting for the safety tick. The call is a few
        // atomic operations and must stay that way.
        self.topology_trigger.note_output();
        // Both survive the record they describe, so a slow leg can name its
        // pane and its size. The pane id is a tmux ordinal — one short-string
        // clone per emitted batch, which is roughly one per control-stream
        // read, on a path that already copies the batch itself into the pane's
        // recovery material.
        let leg_bytes = data.len();
        let leg_pane_id = pane_id.clone();
        let (admitted, leg) = {
            let _emission = self.emission_order.lock().unwrap();
            let generation = self.terminal_generation.fetch_add(1, Ordering::AcqRel) + 1;
            let (visible, degradations) =
                with_active_resources(self.resources, self.stopped, |resources| {
                    let visible = resources.record_output(&pane_id, &data, generation)
                        == OutputDisposition::Visible;
                    // Recording output is what pushes the store past its
                    // budgets, so it is also where a pane — this one or another
                    // — loses its recovery material. Taken here and reported
                    // below, with the store's lock released.
                    (visible, resources.take_degradations())
                })
                .unwrap_or((false, Vec::new()));
            emit_pane_degradations(self.sender, self.overflowed, degradations);
            // Resource ownership is released before channel backpressure. The
            // emission fence stays held so a reveal transition and its recovery
            // event cannot be overtaken by output that observes Visible.
            if visible {
                crate::diagnostics::note_terminal_output_admitted(
                    self.connection_epoch.get(),
                    &leg_pane_id,
                    generation,
                    self.read_started.elapsed(),
                );
            }
            let admitted = visible
                && emit_terminal(
                    self.sender,
                    self.overflowed,
                    v1::EventKind::TerminalOutput,
                    pane_id,
                    data,
                    generation,
                    self.stopped,
                    self.output_credit,
                );
            // The end of the output leg, read here rather than after the fence
            // because everything the leg covers has now happened and nothing
            // else has: `admit` and the sequencer's own backpressure are part of
            // it, the delivery window below is not. A record nothing admitted
            // has no leg — there is no emission to have been slow.
            let leg = admitted.then(|| self.read_started.elapsed());
            if let Some(elapsed) = leg {
                crate::diagnostics::update_terminal_output_admitted(
                    self.connection_epoch.get(),
                    &leg_pane_id,
                    generation,
                    elapsed,
                );
            } else if visible {
                crate::diagnostics::forget_terminal_output_admitted(
                    self.connection_epoch.get(),
                    &leg_pane_id,
                    generation,
                );
            }
            (admitted, leg)
        };
        // Reported with the fence released: the measurement is two instructions
        // and belongs under it, writing a log line is I/O and does not.
        if let Some(elapsed) = leg {
            crate::diagnostics::record_slow_output_leg(elapsed, leg_bytes, &leg_pane_id);
        }
        // Flow control, after the fence: this reader waits for its own record
        // to fit the window, holding nothing. Waiting under the fence is what
        // froze every pane — and, through `set_visibility`, the terminal mutex
        // behind it. Output for a hidden pane admits nothing and so waits for
        // nothing; it keeps feeding the pane's recovery material as before.
        if admitted {
            self.output_credit.await_window(self.stopped);
        }
    }
}

/// The pane a `%pause`/`%continue` names, if it names a valid one.
pub(super) fn notification_pane(arguments: &str) -> Option<String> {
    let pane_id = arguments.split_whitespace().next()?;
    validate_tmux_id(pane_id, '%').ok()?;
    Some(pane_id.to_owned())
}

pub(super) fn is_topology_notification(name: &str) -> bool {
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
