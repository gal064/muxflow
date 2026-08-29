//! Reporting the pane-resource decisions nobody asked for.
//!
//! [`tmux_control::PaneResourceStore`] evicts and releases panes on its own,
//! under a global byte budget and a per-pane tail budget. Both discards leave
//! the pane needing an authoritative seed, and both used to happen in silence:
//! the desktop went on rendering a pane whose recovery material the host had
//! already thrown away, and nothing on either side ever said so. The store
//! records what it discarded; this is where that record becomes the same
//! `PaneResource` event the visibility handoff already sends.

use std::sync::{Mutex, atomic::AtomicBool};

use tmux_agent_protocol::v1;
use tmux_control::{
    PaneDegradation, PaneDegradationCause, PaneResource, PaneResourceState, PaneResourceStore,
};
use tokio::sync::mpsc;

use super::super::{SequencerControl, emit_event};
use super::OutputCharge;

pub(super) fn protocol_state(state: PaneResourceState) -> i32 {
    match state {
        PaneResourceState::Visible => v1::PaneResourceState::Visible.into(),
        PaneResourceState::HiddenBuffered => v1::PaneResourceState::HiddenBuffered.into(),
        PaneResourceState::Released => v1::PaneResourceState::Released.into(),
    }
}

/// The one event shape that tells the desktop what a pane's host-side resource
/// is and whether it owes that pane a seed.
pub(super) fn pane_resource_event(
    pane_id: &str,
    resource: PaneResource,
    charge: OutputCharge,
) -> v1::HostEvent {
    v1::HostEvent {
        kind: v1::EventKind::PaneResource.into(),
        scope: pane_id.into(),
        pane_resource: Some(v1::PaneResource {
            pane_id: pane_id.into(),
            state: protocol_state(resource.state),
            // Kept on the wire for one release so a desktop that still reads it
            // finds a field rather than a gap. Nothing writes it: the renderer
            // owns its screen, and this host answers with a verified tail or
            // with a seed.
            serialized_snapshot: Vec::new(),
            raw_tail: resource.raw_tail,
            generation: resource.generation,
            snapshot_generation: resource.snapshot_generation,
            tail_through_generation: resource.tail_through_generation,
            requires_seed: resource.requires_seed,
            resume_from_renderer: resource.resume_from_renderer,
            recovery_reason: resource.recovery_reason,
        }),
        terminal_delivery_bytes: charge.bytes,
        terminal_delivery_records: charge.records,
        ..Default::default()
    }
}

/// Takes what the store discarded, holding its lock for that and nothing else.
///
/// Emission is deliberately not done here. The event path takes the ordered
/// sequencer — and, for the visibility handoff, the delivery credit — and a
/// caller that reported degradations while still holding the pane-resource
/// store would be holding it across exactly those waits. That is the same rule
/// the visibility handoff boundary states for its own transition, and it is why
/// the store records instead of emitting.
pub(super) fn take_pane_degradations(resources: &Mutex<PaneResourceStore>) -> Vec<PaneDegradation> {
    resources.lock().unwrap().take_degradations()
}

/// Reports each discard as the `requiresSeed` signal the desktop already acts
/// on.
///
/// Carries no recovery bytes, because there are none left: that is what a
/// degradation is. The charge is therefore zero, and the event travels the
/// ordinary queue rather than the credit path — a recovery-class event, which
/// [`crate::service::emit_event`] defers rather than drops.
///
/// A hidden pane outgrowing its tail bound is counted and not reported. It is
/// what a busy background pane does, not a fault: its reveal is answered with a
/// photograph, which is fresher than the tail would have been, and telling the
/// desktop about it would put an ordered pane-resource event in front of every
/// switch for every noisy pane — the traffic the bound exists to remove. An
/// eviction is the opposite: nobody asked for it and nothing else will mention
/// it, so it is still spoken.
pub(super) fn emit_pane_degradations(
    sender: &mpsc::Sender<SequencerControl>,
    overflowed: &AtomicBool,
    degradations: Vec<PaneDegradation>,
) {
    for degradation in degradations {
        crate::diagnostics::record_pane_degradation(
            degradation.cause == PaneDegradationCause::GlobalBudget,
            degradation.state == PaneResourceState::Released,
        );
        if degradation.cause == PaneDegradationCause::HiddenTailOverflow {
            continue;
        }
        emit_event(
            sender,
            overflowed,
            pane_resource_event(
                &degradation.pane_id,
                PaneResource {
                    state: degradation.state,
                    raw_tail: Vec::new(),
                    generation: degradation.generation,
                    snapshot_generation: degradation.generation,
                    tail_through_generation: degradation.generation,
                    requires_seed: true,
                    resume_from_renderer: false,
                    recovery_reason: degradation.reason,
                },
                OutputCharge::default(),
            ),
        );
    }
}

/// Takes and reports in one step, for the callers that hold no other lock.
pub(super) fn report_pane_degradations(
    resources: &Mutex<PaneResourceStore>,
    sender: &mpsc::Sender<SequencerControl>,
    overflowed: &AtomicBool,
) {
    emit_pane_degradations(sender, overflowed, take_pane_degradations(resources));
}
