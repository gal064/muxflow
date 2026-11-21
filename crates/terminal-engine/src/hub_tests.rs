//! Port of `TerminalEventHub.test.ts`. Cases that exercised a callback throwing
//! across the hub have no equivalent: effects cannot throw.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use serde_json::json;

use super::*;
use crate::event::{PaneGrid, PaneResourceState};

fn output(sequence: u64, generation: u64, pane: &str, data: &[u8]) -> TerminalEvent {
    TerminalEvent::Pane(PaneEvent::Output {
        pane_id: pane.into(),
        generation,
        data: data.to_vec(),
        sequence,
    })
}

fn out1(sequence: u64, generation: u64) -> TerminalEvent {
    output(sequence, generation, "%1", &[generation as u8])
}

fn seed(sequence: u64, generation: u64, pane: &str, data: &[u8]) -> TerminalEvent {
    TerminalEvent::Pane(PaneEvent::Seed {
        pane_id: pane.into(),
        generation,
        data: data.to_vec(),
        sequence,
    })
}

fn seed1(sequence: u64, generation: u64) -> TerminalEvent {
    seed(sequence, generation, "%1", &[generation as u8])
}

fn base_resource(sequence: u64, generation: u64) -> PaneResource {
    PaneResource {
        pane_id: "%1".into(),
        state: PaneResourceState::HiddenBuffered,
        requires_seed: false,
        resume_from_renderer: false,
        recovery_reason: String::new(),
        generation,
        snapshot_generation: generation.saturating_sub(1),
        tail_through_generation: generation,
        raw_tail: vec![generation as u8 + 10],
        sequence,
    }
}

fn resource(sequence: u64, generation: u64) -> TerminalEvent {
    TerminalEvent::Pane(PaneEvent::Resource(base_resource(sequence, generation)))
}

fn resource_with(
    sequence: u64,
    generation: u64,
    edit: impl FnOnce(&mut PaneResource),
) -> TerminalEvent {
    let mut value = base_resource(sequence, generation);
    edit(&mut value);
    TerminalEvent::Pane(PaneEvent::Resource(value))
}

fn empty_seed_debt(sequence: u64, generation: u64) -> TerminalEvent {
    resource_with(sequence, generation, |r| {
        r.requires_seed = true;
        r.recovery_reason = "host recovery pending".into();
        r.raw_tail = Vec::new();
    })
}

fn resume_answer(sequence: u64, generation: u64) -> TerminalEvent {
    resource_with(sequence, generation, |r| {
        r.resume_from_renderer = true;
        r.raw_tail = Vec::new();
    })
}

fn history(sequence: u64) -> TerminalEvent {
    TerminalEvent::Pane(PaneEvent::History {
        pane_id: "%1".into(),
        data: vec![7],
        history_size: None,
        sequence,
    })
}

fn epoch(epoch: u64) -> TerminalEvent {
    TerminalEvent::GenerationEpoch { epoch, sequence: 0 }
}

fn diagnostic(sequence: u64, message: &str) -> TerminalEvent {
    TerminalEvent::Pane(PaneEvent::SeedDiagnostic {
        pane_id: "%1".into(),
        message: message.into(),
        sequence,
    })
}

/// A hub with the bookkeeping the TypeScript tests get from closures.
struct Harness {
    hub: TerminalEventHub,
    now: Instant,
    outputs: Vec<HubOutput>,
    delivered: Vec<PaneEvent>,
    seeds: Vec<(String, String)>,
    incidents: Vec<(&'static str, serde_json::Value)>,
    grids: Vec<(String, Option<GridSize>)>,
    health: Vec<(String, PaneHealth)>,
    epochs: Vec<u64>,
    app: RefCell<usize>,
}

impl Harness {
    fn new() -> Self {
        Self::with(HubLimits::default())
    }

    fn with(limits: HubLimits) -> Self {
        Self {
            hub: TerminalEventHub::new(limits),
            now: Instant::now(),
            outputs: Vec::new(),
            delivered: Vec::new(),
            seeds: Vec::new(),
            incidents: Vec::new(),
            grids: Vec::new(),
            health: Vec::new(),
            epochs: Vec::new(),
            app: RefCell::new(0),
        }
    }

    fn drain(&mut self) {
        for output in self.outputs.drain(..) {
            match output {
                HubOutput::Deliver(event) => self.delivered.push(event),
                HubOutput::SeedRequired { pane_id, reason } => self.seeds.push((pane_id, reason)),
                HubOutput::Incident { kind, detail } => self.incidents.push((kind, detail)),
                HubOutput::Grid { pane_id, size } => self.grids.push((pane_id, size)),
                HubOutput::Health { pane_id, health } => self.health.push((pane_id, health)),
                HubOutput::Epoch(epoch) => self.epochs.push(epoch),
                HubOutput::DeliverToApp => *self.app.borrow_mut() += 1,
                HubOutput::Repaint { .. } => {}
            }
        }
    }

    fn publish(&mut self, event: TerminalEvent) -> Admission {
        let admission = self.hub.publish(event, self.now, &mut self.outputs);
        self.drain();
        admission
    }

    fn subscribe(&mut self, pane: &str) {
        self.hub.subscribe_pane(pane, &mut self.outputs).unwrap();
        self.drain();
    }

    fn unsubscribe(&mut self, pane: &str) {
        self.hub.unsubscribe_pane(pane, &mut self.outputs);
        self.drain();
    }

    fn take_delivered(&mut self) -> Vec<PaneEvent> {
        std::mem::take(&mut self.delivered)
    }

    fn seed_panes(&self) -> Vec<&str> {
        self.seeds.iter().map(|(pane, _)| pane.as_str()).collect()
    }

    fn mark_rendered(&mut self, pane: &str, generation: u64, terminal_epoch: Option<u64>) {
        self.hub
            .mark_rendered(pane, generation, terminal_epoch, &mut self.outputs);
        self.drain();
    }
}

fn pane(event: TerminalEvent) -> PaneEvent {
    match event {
        TerminalEvent::Pane(event) => event,
        other => panic!("not a pane event: {other:?}"),
    }
}

fn kinds(events: &[PaneEvent]) -> Vec<&'static str> {
    events
        .iter()
        .map(|event| match event {
            PaneEvent::Seed { .. } => "seed",
            PaneEvent::Output { .. } => "output",
            PaneEvent::Resource(_) => "paneResource",
            PaneEvent::SeedDiagnostic { .. } => "seedDiagnostic",
            PaneEvent::History { .. } => "terminalHistory",
        })
        .collect()
}

#[test]
fn replays_byte_exact_hidden_output_when_a_pane_becomes_visible() {
    let mut h = Harness::new();
    h.publish(output(1, 1, "%1", &[0, 255, 27]));
    h.subscribe("%1");
    assert_eq!(
        h.take_delivered(),
        vec![pane(output(1, 1, "%1", &[0, 255, 27]))]
    );
}

#[test]
fn replaces_stale_hidden_output_when_a_fresh_seed_arrives() {
    let mut h = Harness::new();
    h.publish(out1(1, 1));
    h.publish(seed1(2, 2));
    h.subscribe("%1");
    assert_eq!(h.take_delivered(), vec![pane(seed1(2, 2))]);
}

#[test]
fn asks_for_a_usable_seed_instead_of_silently_dropping_one_that_looks_stale() {
    let mut h = Harness::new();
    h.publish(out1(1, 7));
    h.publish(seed1(2, 3));
    h.subscribe("%1");
    assert_eq!(h.take_delivered(), vec![pane(out1(1, 7))]);
    assert_eq!(h.seed_panes(), ["%1"]);
    h.publish(seed1(3, 4));
    assert_eq!(h.seed_panes(), ["%1"]);
    h.publish(seed1(4, 9));
    h.publish(seed1(5, 5));
    assert_eq!(h.seed_panes(), ["%1", "%1"]);
}

#[test]
fn does_not_replay_output_already_delivered_to_a_mounted_pane() {
    let mut h = Harness::new();
    h.subscribe("%1");
    h.publish(out1(1, 1));
    h.unsubscribe("%1");
    h.take_delivered();
    h.subscribe("%1");
    assert_eq!(h.take_delivered(), vec![]);
}

#[test]
fn enforces_one_ownership_consumer_for_each_pane() {
    let mut h = Harness::new();
    h.subscribe("%1");
    assert_eq!(
        h.hub.subscribe_pane("%1", &mut h.outputs).unwrap_err(),
        "terminal pane %1 already has an active consumer"
    );
    h.unsubscribe("%1");
    h.hub.subscribe_pane("%1", &mut h.outputs).unwrap();
}

#[test]
fn drops_hidden_state_and_old_generation_watermarks_at_a_new_terminal_epoch() {
    let mut h = Harness::new();
    h.publish(epoch(1));
    h.publish(out1(1, 9_000));
    h.publish(epoch(2));
    h.publish(seed1(1, 1));
    h.subscribe("%1");
    assert_eq!(h.take_delivered(), vec![pane(seed1(1, 1))]);
}

#[test]
fn delivers_dedicated_epoch_subscriptions_only_for_admitted_epoch_frames() {
    let mut h = Harness::new();
    h.publish(out1(1, 1));
    h.publish(epoch(41));
    assert_eq!(*h.app.borrow(), 2, "the output frame and the new epoch");
    h.publish(out1(1, 1));
    h.publish(epoch(41));
    assert_eq!(
        *h.app.borrow(),
        3,
        "a repeated epoch does not reach the app"
    );
    assert_eq!(h.publish(out1(2, 2)), Admission::Accepted);
    assert_eq!(h.epochs, [41]);
}

#[test]
fn ignores_duplicate_terminal_generations_and_waits_for_seed_after_recovery_invalidation() {
    let mut h = Harness::new();
    h.subscribe("%1");
    h.publish(out1(1, 5));
    h.publish(output(2, 5, "%1", &[9]));
    h.publish(resource_with(3, 6, |r| {
        r.state = PaneResourceState::Released;
        r.requires_seed = true;
        r.recovery_reason = "overflow".into();
        r.raw_tail = Vec::new();
    }));
    h.publish(out1(4, 7));
    h.publish(seed1(5, 8));
    h.publish(out1(6, 9));
    assert_eq!(
        kinds(&h.delivered),
        ["output", "paneResource", "seed", "output"]
    );
}

#[test]
fn requests_one_scoped_seed_on_a_bounded_hidden_backlog_overflow_and_suppresses_until_that_seed() {
    let mut h = Harness::with(HubLimits {
        max_pane_bytes: 8,
        max_total_bytes: 16,
        ..HubLimits::default()
    });
    h.publish(output(1, 1, "%7", &[0; 8]));
    h.publish(output(2, 2, "%7", &[2]));
    h.publish(output(3, 3, "%7", &[3]));
    assert_eq!(
        h.seeds,
        [(
            "%7".to_owned(),
            "frontend hidden recovery buffer exceeded 8 bytes".to_owned()
        )]
    );
    h.publish(seed(4, 4, "%7", &[4]));
    h.publish(output(5, 5, "%7", &[5]));
    h.subscribe("%7");
    assert_eq!(
        h.take_delivered(),
        vec![pane(seed(4, 4, "%7", &[4])), pane(output(5, 5, "%7", &[5]))]
    );
}

#[test]
fn bounds_zero_byte_hidden_records_as_well_as_retained_bytes() {
    let mut h = Harness::with(HubLimits {
        max_pane_events: 2,
        ..HubLimits::default()
    });
    h.publish(output(1, 1, "%7", &[]));
    h.publish(output(2, 2, "%7", &[]));
    h.publish(output(3, 3, "%7", &[]));
    assert_eq!(h.seed_panes(), ["%7"]);
    assert_eq!(h.hub.retained_byte_length(), 0);
    assert_eq!(h.hub.retained_pane_count(), 0);
}

#[test]
fn bounds_a_hidden_resources_recovery_reason_even_when_its_byte_segments_are_empty() {
    let mut h = Harness::with(HubLimits {
        max_pane_bytes: 4,
        ..HubLimits::default()
    });
    h.publish(resource_with(1, 1, |r| {
        r.recovery_reason = "12345".into();
        r.raw_tail = Vec::new();
    }));
    assert_eq!(h.seed_panes(), ["%1"]);
    assert_eq!(h.hub.retained_byte_length(), 0);
}

#[test]
fn accounts_the_exact_bytes_retained_for_hidden_output() {
    let mut h = Harness::new();
    h.publish(output(1, 1, "%7", &[1, 2, 3]));
    h.publish(output(2, 2, "%7", &[4, 5, 6, 7]));
    assert_eq!(h.hub.retained_byte_length(), 7);
    h.subscribe("%7");
    assert_eq!(h.hub.retained_byte_length(), 0);
}

#[test]
fn replaces_a_consumed_hidden_recovery_checkpoint_instead_of_replaying_its_stale_raw_tail() {
    let mut h = Harness::new();
    h.publish(resource(1, 1));
    h.publish(resource(2, 2));
    h.subscribe("%1");
    assert_eq!(h.take_delivered(), vec![pane(resource(2, 2))]);
}

#[test]
fn keeps_exact_byte_accounting_while_replacing_resource_and_diagnostic_boundaries() {
    let mut h = Harness::new();
    h.publish(output(1, 1, "%1", &[1, 2, 3]));
    h.publish(diagnostic(2, "old"));
    h.publish(resource_with(3, 2, |r| r.raw_tail = Vec::new()));
    h.publish(diagnostic(4, "λ"));
    assert_eq!(h.hub.retained_byte_length(), 5);
    let final_resource = || {
        resource_with(5, 3, |r| {
            r.recovery_reason = "λ".into();
            r.raw_tail = vec![7, 8, 9];
        })
    };
    h.publish(final_resource());
    assert_eq!(h.hub.retained_byte_length(), 5);
    h.subscribe("%1");
    assert_eq!(h.take_delivered(), vec![pane(final_resource())]);
    assert_eq!(h.hub.retained_byte_length(), 0);
}

#[test]
fn routes_seed_diagnostics_only_to_their_pane_without_requesting_recovery() {
    let mut h = Harness::new();
    h.subscribe("%1");
    h.publish(diagnostic(1, "partial alternate metadata"));
    assert_eq!(
        h.take_delivered(),
        vec![pane(diagnostic(1, "partial alternate metadata"))]
    );
    assert!(h.seeds.is_empty());
}

#[test]
fn admits_a_forward_sequence_jump_carries_the_watermark_and_reseeds_the_mounted_panes_once() {
    let mut h = Harness::new();
    h.subscribe("%1");
    h.subscribe("%2");
    assert_eq!(h.publish(out1(1, 1)), Admission::Accepted);
    assert!(h.seeds.is_empty());
    assert!(h.incidents.is_empty());
    assert_eq!(h.publish(out1(4, 2)), Admission::Accepted);
    assert_eq!(h.publish(out1(9, 3)), Admission::Accepted);
    assert_eq!(h.publish(out1(20, 4)), Admission::Accepted);
    assert_eq!(h.hub.last_sequence(), 20);
    assert_eq!(
        h.delivered,
        vec![
            pane(out1(1, 1)),
            pane(out1(4, 2)),
            pane(out1(9, 3)),
            pane(out1(20, 4))
        ]
    );
    let reason = "event sequence jumped (decode drop?)";
    assert_eq!(
        h.seeds,
        [
            ("%1".to_owned(), reason.to_owned()),
            ("%2".to_owned(), reason.to_owned())
        ]
    );
    assert_eq!(
        h.incidents,
        [("link.eventGap", json!({ "expected": 2, "received": 4 }))]
    );

    assert_eq!(h.publish(out1(21, 5)), Admission::Accepted);
    assert_eq!(h.seeds.len(), 2);
    assert_eq!(h.incidents.len(), 1);
    assert_eq!(h.publish(out1(20, 6)), Admission::Stale);
    assert_eq!(h.delivered.len(), 5);

    // Past the debounce a later jump is a new anomaly.
    h.now += Duration::from_millis(1_001);
    h.publish(out1(40, 7));
    assert_eq!(h.seeds.len(), 4);
}

fn snapshot(sequence: u64, authoritative: bool, columns: usize, rows: usize) -> TerminalEvent {
    TerminalEvent::Snapshot {
        grids: Some(vec![PaneGrid {
            pane_id: "%1".into(),
            size: GridSize { columns, rows },
        }]),
        authoritative,
        sequence,
    }
}

fn layout(sequence: u64, columns: usize, rows: usize) -> TerminalEvent {
    TerminalEvent::TopologyDirty {
        grids: Some(vec![PaneGrid {
            pane_id: "%1".into(),
            size: GridSize { columns, rows },
        }]),
        sequence,
    }
}

#[test]
fn replaces_missed_layout_dimensions_at_the_authoritative_resync_boundary_before_its_seed() {
    let mut h = Harness::new();
    h.subscribe("%1");
    h.publish(layout(1, 40, 12));
    h.publish(snapshot(2, false, 80, 24));
    let size = |columns, rows| ("%1".to_owned(), Some(GridSize { columns, rows }));
    assert_eq!(h.grids, [size(40, 12)]);
    h.publish(snapshot(77, true, 80, 24));
    h.publish(seed1(78, 3));
    assert_eq!(h.grids, [size(40, 12), size(80, 24)]);
    assert_eq!(kinds(&h.delivered), ["seed"]);
}

#[test]
fn fast_forwards_to_an_authoritative_snapshot_without_calling_a_jump_a_jump() {
    let mut h = Harness::new();
    h.subscribe("%1");
    h.publish(out1(1, 1));
    let barrier = TerminalEvent::Snapshot {
        grids: Some(Vec::new()),
        authoritative: true,
        sequence: 77,
    };
    assert_eq!(h.publish(barrier), Admission::Accepted);
    assert_eq!(h.hub.last_sequence(), 77);
    assert_eq!(h.publish(out1(78, 2)), Admission::Accepted);
    assert!(h.seeds.is_empty());
    assert!(h.incidents.is_empty());
}

#[test]
fn reseeds_every_mounted_pane_on_request_reopening_a_one_shot_conflict_latch() {
    let mut h = Harness::new();
    h.subscribe("%1");
    h.subscribe("%2");
    h.publish(out1(1, 7));
    h.publish(seed1(2, 3));
    assert_eq!(
        h.seeds,
        [(
            "%1".to_owned(),
            "stale or conflicting terminal visibility handoff".to_owned()
        )]
    );
    h.hub
        .reseed_subscribed_panes("post-resync reseed", &mut h.outputs);
    h.drain();
    assert_eq!(
        h.seeds[1..],
        [
            ("%1".to_owned(), "post-resync reseed".to_owned()),
            ("%2".to_owned(), "post-resync reseed".to_owned())
        ]
    );
    assert!(!h.hub.pane_health("%1").conflict_reseed_requested);

    h.unsubscribe("%1");
    h.hub
        .reseed_subscribed_panes("post-resync reseed", &mut h.outputs);
    h.drain();
    assert_eq!(
        h.seeds[3..],
        [("%2".to_owned(), "post-resync reseed".to_owned())]
    );
}

#[test]
fn admits_a_sequence_zero_agent_snapshot_paired_with_an_authoritative_topology_snapshot() {
    let mut h = Harness::new();
    let topology = TerminalEvent::Snapshot {
        grids: Some(Vec::new()),
        authoritative: true,
        sequence: 12,
    };
    assert_eq!(h.publish(topology), Admission::Accepted);
    assert_eq!(
        h.publish(TerminalEvent::Other { sequence: 0 }),
        Admission::Local
    );
    assert_eq!(*h.app.borrow(), 2);
    assert_eq!(h.hub.last_sequence(), 12);
}

#[test]
fn advances_the_watermark_through_a_standalone_agent_event_instead_of_reading_it_as_a_gap() {
    let mut h = Harness::new();
    assert_eq!(h.publish(out1(1, 1)), Admission::Accepted);
    assert_eq!(
        h.publish(TerminalEvent::Other { sequence: 2 }),
        Admission::Accepted
    );
    assert_eq!(h.publish(resource(3, 2)), Admission::Accepted);
    assert_eq!(*h.app.borrow(), 3);
    assert_eq!(h.hub.last_sequence(), 3);
    assert!(h.incidents.is_empty());
}

#[test]
fn treats_repeated_checkpoints_as_idempotent_but_scoped_reseeds_a_conflicting_handoff_without_discarding_backlog()
 {
    let mut h = Harness::new();
    h.publish(out1(1, 1));
    h.publish(resource(2, 2));
    h.publish(resource(3, 2));
    assert!(h.seeds.is_empty());
    h.publish(resource_with(4, 2, |r| r.raw_tail = vec![99]));
    assert_eq!(h.seed_panes(), ["%1"]);
    h.subscribe("%1");
    assert_eq!(h.take_delivered(), vec![pane(resource(2, 2))]);
}

#[test]
fn uses_conservative_reseed_after_transferring_a_resource_to_an_active_pane() {
    let mut h = Harness::new();
    h.subscribe("%1");
    h.publish(resource(1, 2));
    assert_eq!(h.hub.retained_byte_length(), 0);
    h.publish(resource_with(2, 2, |r| r.raw_tail = vec![99]));
    assert_eq!(h.seed_panes(), ["%1"]);
}

#[test]
fn retains_one_exclusively_owned_dormant_resource_payload() {
    let mut h = Harness::new();
    h.publish(resource(1, 2));
    assert_eq!(h.hub.retained_byte_length(), 1);
    assert_eq!(h.hub.retained_pane_count(), 1);
    h.subscribe("%1");
    assert_eq!(h.hub.retained_byte_length(), 0);
    assert_eq!(h.hub.retained_pane_count(), 0);
}

#[test]
fn requires_a_seed_after_a_consumer_cannot_apply_an_admitted_event() {
    let mut h = Harness::new();
    h.subscribe("%1");
    h.publish(out1(1, 1));
    // What the engine does when applying that event to the terminal failed.
    h.hub.require_seed(
        "%1",
        "terminal pane consumer rejected an admitted event",
        &mut h.outputs,
    );
    h.drain();
    assert_eq!(h.health.last().unwrap(), &awaiting("%1"));
    assert_eq!(h.publish(out1(2, 2)), Admission::Accepted);
    assert_eq!(h.take_delivered(), vec![pane(out1(1, 1))]);
    assert_eq!(h.seed_panes(), ["%1"]);
}

#[test]
fn requires_one_seed_when_a_replayed_backlog_cannot_be_applied() {
    let mut h = Harness::new();
    h.publish(out1(1, 1));
    h.publish(out1(2, 2));
    h.subscribe("%1");
    h.take_delivered();
    h.hub.require_seed(
        "%1",
        "terminal pane consumer rejected backlog replay",
        &mut h.outputs,
    );
    h.drain();
    h.publish(out1(3, 3));
    assert_eq!(h.take_delivered(), vec![]);
    h.publish(seed1(4, 4));
    assert_eq!(h.take_delivered(), vec![pane(seed1(4, 4))]);
    assert_eq!(h.seed_panes(), ["%1"]);
}

#[test]
fn conservatively_reseeds_an_oversized_same_generation_checkpoint() {
    let mut h = Harness::new();
    h.subscribe("%1");
    let oversized = || resource_with(1, 2, |r| r.raw_tail = vec![0; 256 * 1024 + 1]);
    h.publish(oversized());
    let TerminalEvent::Pane(PaneEvent::Resource(mut again)) = oversized() else {
        unreachable!()
    };
    again.sequence = 2;
    h.publish(TerminalEvent::Pane(PaneEvent::Resource(again)));
    assert_eq!(h.seed_panes(), ["%1"]);
}

#[test]
fn keeps_output_arriving_after_hide_serialization_outside_the_exact_cutoff_and_available_for_recovery()
 {
    let mut h = Harness::new();
    h.publish(epoch(42));
    h.subscribe("%1");
    h.publish(out1(1, 7));
    h.mark_rendered("%1", 7, Some(42));
    let serialized = h.hub.visibility_checkpoint("%1");
    h.unsubscribe("%1");
    h.publish(out1(2, 8));
    assert_eq!(
        serialized,
        Some(VisibilityCheckpoint {
            terminal_epoch: 42,
            output_generation: 7
        })
    );
    assert_eq!(
        h.hub.visibility_checkpoint("%1").unwrap().output_generation,
        7
    );
    h.take_delivered();
    h.subscribe("%1");
    assert_eq!(h.take_delivered(), vec![pane(out1(2, 8))]);
}

#[test]
fn invalidates_rendered_cutoffs_across_a_terminal_epoch_mismatch() {
    let mut h = Harness::new();
    h.publish(epoch(41));
    h.mark_rendered("%1", 99, Some(41));
    assert_eq!(
        h.hub.visibility_checkpoint("%1").unwrap().output_generation,
        99
    );
    h.publish(epoch(42));
    assert_eq!(
        h.hub.visibility_checkpoint("%1"),
        Some(VisibilityCheckpoint {
            terminal_epoch: 42,
            output_generation: 0
        })
    );
}

#[test]
fn ignores_a_rendered_generation_from_another_epoch() {
    let mut h = Harness::new();
    h.publish(epoch(41));
    h.mark_rendered("%1", 5, Some(40));
    h.mark_rendered("%1", 5, None);
    assert_eq!(
        h.hub.visibility_checkpoint("%1").unwrap().output_generation,
        0
    );
}

#[test]
fn bounds_aggregate_hidden_bytes_and_pane_metadata_across_many_panes() {
    let mut h = Harness::with(HubLimits {
        max_pane_bytes: 16,
        max_total_bytes: 24,
        max_buffered_panes: 3,
        max_tracked_panes: 5,
        ..HubLimits::default()
    });
    for pane_number in 1..=20 {
        h.publish(seed(pane_number, 1, &format!("%{pane_number}"), &[0; 8]));
    }
    assert!(h.hub.retained_pane_count() <= 3);
    assert!(h.hub.retained_byte_length() <= 24);
    assert!(h.hub.tracked_pane_count() <= 5);
    assert!(!h.seeds.is_empty());
    h.subscribe("%20");
    assert_eq!(h.take_delivered(), vec![pane(seed(20, 1, "%20", &[0; 8]))]);
}

#[test]
fn preserves_seed_debt_when_a_hidden_backlog_is_evicted_from_the_metadata_lru() {
    let mut h = Harness::with(HubLimits {
        max_tracked_panes: 1,
        max_buffered_panes: 2,
        ..HubLimits::default()
    });
    h.publish(output(1, 1, "%1", &[1]));
    h.publish(output(2, 1, "%2", &[1]));
    assert_eq!(h.seed_panes(), ["%1"]);

    h.publish(output(3, 2, "%1", &[2]));
    h.subscribe("%1");
    assert_eq!(h.take_delivered(), vec![]);
    assert_eq!(h.seed_panes(), ["%1", "%2"]);

    h.publish(seed(4, 3, "%1", &[3]));
    h.publish(output(5, 4, "%1", &[4]));
    assert_eq!(
        h.take_delivered(),
        vec![pane(seed(4, 3, "%1", &[3])), pane(output(5, 4, "%1", &[4]))]
    );
}

#[test]
fn pins_an_active_panes_generation_resource_identity_and_rendered_checkpoint() {
    let mut h = Harness::with(HubLimits {
        max_tracked_panes: 1,
        max_buffered_panes: 2,
        ..HubLimits::default()
    });
    h.publish(epoch(7));
    h.subscribe("%1");
    h.publish(seed(1, 5, "%1", &[5]));
    h.mark_rendered("%1", 5, Some(7));

    h.publish(output(2, 1, "%2", &[1]));
    h.publish(output(3, 1, "%3", &[1]));
    h.publish(output(4, 4, "%1", &[4]));

    assert_eq!(h.take_delivered(), vec![pane(seed(1, 5, "%1", &[5]))]);
    assert_eq!(
        h.hub.visibility_checkpoint("%1"),
        Some(VisibilityCheckpoint {
            terminal_epoch: 7,
            output_generation: 5
        })
    );
    assert_eq!(h.seed_panes(), ["%2"]);
}

#[test]
fn keeps_mounted_panes_outside_the_dormant_lru_capacity_and_hot_path() {
    let mut h = Harness::with(HubLimits {
        max_tracked_panes: 1,
        max_buffered_panes: 1,
        ..HubLimits::default()
    });
    h.publish(epoch(9));
    let mut received: BTreeMap<String, usize> = BTreeMap::new();
    for index in 1..=32_u64 {
        let pane_id = format!("%{index}");
        h.subscribe(&pane_id);
        h.publish(seed(index, index, &pane_id, &[index as u8]));
        h.mark_rendered(&pane_id, index, Some(9));
        *received.entry(pane_id).or_default() += h.take_delivered().len();
    }
    assert!(h.seeds.is_empty());
    assert_eq!(received.len(), 32);
    assert_eq!(h.hub.tracked_pane_count(), 32);
    assert_eq!(
        h.hub.visibility_checkpoint("%1").unwrap().output_generation,
        1
    );
    assert_eq!(
        h.hub
            .visibility_checkpoint("%32")
            .unwrap()
            .output_generation,
        32
    );
}

#[test]
fn requires_a_seed_conservatively_after_the_bounded_debt_tombstone_ages_out() {
    let mut h = Harness::with(HubLimits {
        max_tracked_panes: 1,
        max_buffered_panes: 3,
        ..HubLimits::default()
    });
    h.publish(output(1, 1, "%1", &[1]));
    h.publish(output(2, 1, "%2", &[1]));
    h.publish(output(3, 1, "%3", &[1]));
    h.publish(output(4, 2, "%1", &[2]));
    h.subscribe("%1");
    assert_eq!(h.take_delivered(), vec![]);
    assert_eq!(h.seed_panes(), ["%1", "%2", "%3", "%1"]);
}

#[test]
fn clamps_a_zero_metadata_capacity_before_it_can_truncate_a_pane_backlog() {
    let mut h = Harness::with(HubLimits {
        max_tracked_panes: 0,
        ..HubLimits::default()
    });
    h.publish(output(1, 1, "%1", &[1]));
    h.publish(output(2, 2, "%1", &[2]));
    h.subscribe("%1");
    assert_eq!(
        h.take_delivered(),
        vec![
            pane(output(1, 1, "%1", &[1])),
            pane(output(2, 2, "%1", &[2]))
        ]
    );
    assert!(h.seeds.is_empty());
    assert_eq!(h.hub.tracked_pane_count(), 1);
}

fn healthy(pane: &str) -> (String, PaneHealth) {
    (pane.to_owned(), PaneHealth::default())
}

fn awaiting(pane: &str) -> (String, PaneHealth) {
    (
        pane.to_owned(),
        PaneHealth {
            awaiting_seed: true,
            conflict_reseed_requested: false,
        },
    )
}

#[test]
fn reports_the_degraded_state_it_is_holding_a_pane_in() {
    let mut h = Harness::new();
    h.subscribe("%1");
    assert_eq!(h.health, [healthy("%1")]);
    h.publish(resource_with(1, 1, |r| {
        r.state = PaneResourceState::Released;
        r.requires_seed = true;
        r.recovery_reason = "host recovery pending".into();
        r.raw_tail = Vec::new();
    }));
    assert_eq!(h.hub.pane_health("%1"), awaiting("%1").1);
    assert_eq!(h.health.last().unwrap(), &awaiting("%1"));

    h.publish(out1(2, 2));
    assert_eq!(h.health.len(), 2);

    h.publish(seed1(3, 3));
    assert_eq!(h.hub.pane_health("%1"), PaneHealth::default());
    assert_eq!(h.health.last().unwrap(), &healthy("%1"));
}

#[test]
fn reports_a_pane_mounting_straight_into_inherited_seed_debt() {
    let mut h = Harness::new();
    h.publish(resource_with(1, 1, |r| {
        r.state = PaneResourceState::Released;
        r.requires_seed = true;
        r.recovery_reason = "host recovery pending".into();
        r.raw_tail = Vec::new();
    }));
    h.subscribe("%1");
    assert_eq!(h.health, [awaiting("%1")]);
}

#[test]
fn makes_the_one_shot_conflict_reseed_retriable() {
    let mut h = Harness::new();
    h.subscribe("%1");
    h.publish(out1(1, 7));
    h.publish(seed1(2, 3));
    assert_eq!(h.seed_panes(), ["%1"]);
    assert!(h.hub.pane_health("%1").conflict_reseed_requested);

    h.publish(seed1(3, 4));
    assert_eq!(h.seed_panes(), ["%1"]);

    h.hub.retry_pane_seed("%1", &mut h.outputs);
    h.drain();
    assert!(!h.hub.pane_health("%1").conflict_reseed_requested);
    assert!(!h.health.last().unwrap().1.conflict_reseed_requested);
    h.publish(seed1(4, 5));
    assert_eq!(h.seed_panes(), ["%1", "%1"]);

    h.publish(seed1(5, 9));
    h.hub.retry_pane_seed("%1", &mut h.outputs);
    h.hub.retry_pane_seed("%unknown", &mut h.outputs);
    h.drain();
    assert_eq!(h.seed_panes(), ["%1", "%1"]);
}

#[test]
fn clears_seed_debt_for_a_zero_byte_answer_only_when_it_carries_the_resume_flag() {
    let mut h = Harness::new();
    h.subscribe("%1");
    h.publish(empty_seed_debt(1, 1));
    assert!(h.hub.pane_health("%1").awaiting_seed);

    h.publish(resource_with(2, 2, |r| r.raw_tail = Vec::new()));
    assert!(h.hub.pane_health("%1").awaiting_seed);

    h.publish(resume_answer(3, 3));
    assert!(!h.hub.pane_health("%1").awaiting_seed);
}

#[test]
fn delivers_a_history_answer_beside_the_output_stream_rather_than_inside_it() {
    let mut h = Harness::new();
    h.subscribe("%1");
    h.publish(seed1(1, 5));
    h.take_delivered();

    h.publish(history(2));
    assert_eq!(kinds(&h.delivered), ["terminalHistory"]);
    h.publish(out1(3, 6));
    assert_eq!(kinds(&h.delivered), ["terminalHistory", "output"]);

    h.publish(resource_with(4, 7, |r| {
        r.requires_seed = true;
        r.recovery_reason = "host recovery pending".into();
    }));
    assert!(h.hub.pane_health("%1").awaiting_seed);
    h.publish(history(5));
    assert!(h.hub.pane_health("%1").awaiting_seed);
}

#[test]
fn drops_a_history_answer_for_a_pane_nothing_is_rendering() {
    let mut h = Harness::new();
    h.subscribe("%1");
    h.unsubscribe("%1");
    h.publish(history(1));
    h.subscribe("%1");
    h.publish(out1(2, 1));
    assert_eq!(kinds(&h.delivered), ["output"]);
}

#[test]
fn does_not_issue_a_redundant_conservative_request_for_an_untracked_repair_event() {
    let repairs: [fn() -> TerminalEvent; 4] = [
        || seed(4, 3, "%1", &[3]),
        || resource(4, 3),
        || empty_seed_debt(4, 3),
        || diagnostic(4, "partial metadata"),
    ];
    for repair in repairs {
        let mut h = Harness::with(HubLimits {
            max_tracked_panes: 1,
            max_buffered_panes: 3,
            ..HubLimits::default()
        });
        h.publish(output(1, 1, "%1", &[1]));
        h.publish(output(2, 1, "%2", &[1]));
        h.publish(output(3, 1, "%3", &[1]));
        h.publish(repair());
        let requests_for_one = h.seeds.iter().filter(|(pane, _)| pane == "%1").count();
        assert_eq!(requests_for_one, 1);
    }
}

#[test]
fn delivers_grid_changes_between_the_surrounding_writes_and_stops_after_unsubscribe() {
    let mut h = Harness::new();
    h.subscribe("%1");
    h.hub.publish(seed1(1, 1), h.now, &mut h.outputs);
    h.hub.publish(out1(2, 2), h.now, &mut h.outputs);
    h.hub.publish(layout(3, 40, 12), h.now, &mut h.outputs);
    h.hub.publish(out1(4, 3), h.now, &mut h.outputs);
    let order: Vec<String> = h
        .outputs
        .iter()
        .filter_map(|output| match output {
            HubOutput::Deliver(event) => Some(kinds(std::slice::from_ref(event))[0].to_owned()),
            HubOutput::Grid {
                size: Some(size), ..
            } => Some(format!("{}x{}", size.columns, size.rows)),
            _ => None,
        })
        .collect();
    assert_eq!(order, ["seed", "output", "40x12", "output"]);
    h.drain();
    h.unsubscribe("%1");
    let grid_count = h.grids.len();
    h.publish(layout(5, 40, 12));
    assert_eq!(h.grids.len(), grid_count);
}

#[test]
fn resets_mounted_grids_and_health_when_pane_state_is_cleared() {
    let mut h = Harness::new();
    h.subscribe("%1");
    h.publish(empty_seed_debt(1, 1));
    h.health.clear();
    h.hub.clear_terminal_state(&mut h.outputs);
    h.drain();
    assert_eq!(h.grids, [("%1".to_owned(), None)]);
    assert_eq!(h.health, [healthy("%1")]);
}

#[test]
fn clearing_one_pane_forgets_its_backlog_and_debt_but_keeps_it_mounted() {
    let mut h = Harness::new();
    h.publish(output(1, 1, "%1", &[1, 2]));
    assert_eq!(h.hub.retained_byte_length(), 2);
    h.hub.clear_pane("%1", &mut h.outputs);
    h.drain();
    assert_eq!(h.hub.retained_byte_length(), 0);
    assert_eq!(h.hub.tracked_pane_count(), 0);

    h.subscribe("%1");
    h.publish(empty_seed_debt(2, 2));
    h.hub.clear_pane("%1", &mut h.outputs);
    h.drain();
    assert_eq!(h.hub.pane_health("%1"), PaneHealth::default());
    h.publish(out1(3, 1));
    assert_eq!(h.delivered.len(), 2, "generation watermark restarted");
}
