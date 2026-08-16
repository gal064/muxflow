use super::*;
use crate::service::terminal::OutputCharge;

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

pub(super) fn emit_terminal(
    sender: &mpsc::Sender<SequencerControl>,
    overflowed: &AtomicBool,
    kind: v1::EventKind,
    pane_id: String,
    data: Vec<u8>,
    generation: u64,
    output_credit: &OutputCredit,
) {
    if matches!(
        kind,
        v1::EventKind::TerminalSeed | v1::EventKind::TerminalOutput
    ) {
        let _ = crate::service::agents::AgentRuntime::global().observe_screen(
            &pane_id,
            &data,
            kind == v1::EventKind::TerminalSeed,
        );
    }
    // Terminal bytes are lossless and already arrive on the dedicated control
    // reader thread. Let the bounded sequencer queue propagate socket pressure
    // back to that reader; tmux can then apply its own pause/continue protocol.
    // A nonblocking send here turned a normal 100 ms / 100 Mbit bandwidth-delay
    // window into a full-connection resync as soon as 1,024 records accumulated.
    let charge = OutputCharge::terminal(data.len());
    let Ok(reservation) = output_credit.reserve(charge) else {
        overflowed.store(true, Ordering::Release);
        return;
    };
    if sender
        .blocking_send(SequencerControl::OrderedEvent(v1::HostEvent {
            kind: kind.into(),
            terminal: Some(v1::TerminalBytes {
                pane_id,
                data,
                generation,
            }),
            terminal_delivery_bytes: charge.bytes,
            terminal_delivery_records: charge.records,
            ..Default::default()
        }))
        .is_err()
    {
        overflowed.store(true, Ordering::Release);
    } else {
        reservation.commit();
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
