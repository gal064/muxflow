//! Routes pane events without retaining an unbounded all-host history. Port of
//! `TerminalEventHub.ts`.
//!
//! A sans-IO state machine: where the TypeScript hub calls back, this one pushes
//! a [`HubOutput`], in the same order. Nothing can throw across it and nothing
//! can reenter it, so the observer-failure containment and the reentrancy copies
//! have no equivalent here.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use indexmap::IndexSet;
use serde_json::{Value, json};

use crate::event::{GridSize, PaneEvent, PaneGrid, PaneResource, TerminalEvent};

/// The degraded states the hub holds a pane in. Both are one-shot latches whose
/// only exit is an authoritative seed, and the pane's own consumer cannot see
/// them: while `awaiting_seed` stands the hub drops that pane's events.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PaneHealth {
    pub awaiting_seed: bool,
    pub conflict_reseed_requested: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VisibilityCheckpoint {
    pub terminal_epoch: u64,
    pub output_generation: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Admission {
    Accepted,
    Local,
    Stale,
}

#[derive(Debug, PartialEq, Eq)]
pub enum HubOutput {
    /// The event is admitted; connection-level consumers may act on it now.
    DeliverToApp,
    Epoch(u64),
    Deliver(PaneEvent),
    Grid {
        pane_id: String,
        size: Option<GridSize>,
    },
    Health {
        pane_id: String,
        health: PaneHealth,
    },
    SeedRequired {
        pane_id: String,
        reason: String,
    },
    /// A pane's screen has just been repainted, for the echo-lag probe.
    Repaint {
        pane_id: String,
        sequence: u64,
        generation: u64,
    },
    Incident {
        kind: &'static str,
        detail: Value,
    },
}

const DEFAULT_MAX_PANE_BYTES: usize = 8 * 1024 * 1024;
const DEFAULT_MAX_TOTAL_BYTES: usize = 16 * 1024 * 1024;
const DEFAULT_MAX_BUFFERED_PANES: usize = 32;
const DEFAULT_MAX_PANE_EVENTS: usize = 65_536;
const MAX_EXACT_RESOURCE_IDENTITY_BYTES: usize = 256 * 1024;

/// A jump is one anomaly however many frames it spans, and the frames arrive in
/// a burst. Repairing per event would ask for the same screens dozens of times.
const SEQUENCE_REPAIR_DEBOUNCE: Duration = Duration::from_millis(1_000);

#[derive(Clone, Copy, Debug)]
pub struct HubLimits {
    pub max_pane_bytes: usize,
    pub max_total_bytes: usize,
    pub max_buffered_panes: usize,
    pub max_tracked_panes: usize,
    pub max_pane_events: usize,
}

impl Default for HubLimits {
    fn default() -> Self {
        Self {
            max_pane_bytes: DEFAULT_MAX_PANE_BYTES,
            max_total_bytes: DEFAULT_MAX_TOTAL_BYTES,
            max_buffered_panes: DEFAULT_MAX_BUFFERED_PANES,
            max_tracked_panes: 64.max(DEFAULT_MAX_BUFFERED_PANES * 4),
            max_pane_events: DEFAULT_MAX_PANE_EVENTS,
        }
    }
}

#[derive(Default)]
struct PaneBacklog {
    entries: Vec<(PaneEvent, usize)>,
    byte_length: usize,
}

impl PaneBacklog {
    fn append(&mut self, event: PaneEvent, byte_length: usize) {
        self.entries.push((event, byte_length));
        self.byte_length += byte_length;
    }

    fn replace(&mut self, event: PaneEvent, byte_length: usize) {
        self.entries.clear();
        self.byte_length = 0;
        self.append(event, byte_length);
    }

    fn remove_kind(&mut self, same_kind: fn(&PaneEvent) -> bool) {
        let mut removed = 0;
        self.entries.retain(|(event, byte_length)| {
            let keep = !same_kind(event);
            if !keep {
                removed += byte_length;
            }
            keep
        });
        self.byte_length -= removed;
    }

    fn latest_resource(&self) -> Option<&PaneResource> {
        self.entries
            .iter()
            .rev()
            .find_map(|(event, _)| match event {
                PaneEvent::Resource(resource) => Some(resource),
                _ => None,
            })
    }
}

struct PaneStreamState {
    backlog: Option<PaneBacklog>,
    /// -1 is "nothing yet", so generation 0 can still be the first one admitted.
    last_generation: i128,
    rendered_generation: u64,
    health: PaneHealth,
}

impl Default for PaneStreamState {
    fn default() -> Self {
        Self {
            backlog: None,
            last_generation: -1,
            rendered_generation: 0,
            health: PaneHealth::default(),
        }
    }
}

/// Every key of `panes` is either subscribed or in the `dormant` LRU, so output
/// for a mounted pane never scans anything.
pub struct TerminalEventHub {
    subscribed: HashSet<String>,
    panes: HashMap<String, PaneStreamState>,
    dormant: IndexSet<String>,
    evicted_seed_debt: IndexSet<String>,
    limits: HubLimits,
    backlog_bytes: usize,
    backlog_count: usize,
    generation_epoch: Option<u64>,
    last_sequence: u64,
    last_sequence_repair_at: Option<Instant>,
    unknown_panes_require_seed: bool,
}

impl Default for TerminalEventHub {
    fn default() -> Self {
        Self::new(HubLimits::default())
    }
}

impl TerminalEventHub {
    pub fn new(mut limits: HubLimits) -> Self {
        // A pane cannot be buffered safely without one metadata slot: otherwise
        // the event that creates its backlog evicts its own generation state.
        limits.max_tracked_panes = limits.max_tracked_panes.max(1);
        Self {
            subscribed: HashSet::new(),
            panes: HashMap::new(),
            dormant: IndexSet::new(),
            evicted_seed_debt: IndexSet::new(),
            limits,
            backlog_bytes: 0,
            backlog_count: 0,
            generation_epoch: None,
            last_sequence: 0,
            last_sequence_repair_at: None,
            unknown_panes_require_seed: false,
        }
    }

    pub fn publish(
        &mut self,
        event: TerminalEvent,
        now: Instant,
        out: &mut Vec<HubOutput>,
    ) -> Admission {
        let new_epoch = match event {
            TerminalEvent::GenerationEpoch { epoch, .. }
                if Some(epoch) != self.generation_epoch =>
            {
                Some(epoch)
            }
            _ => None,
        };
        let admission = self.admit_sequence(&event, now, out);
        if admission == Admission::Stale {
            return admission;
        }
        if let Some(epoch) = new_epoch {
            self.generation_epoch = Some(epoch);
            out.push(HubOutput::Incident {
                kind: "link.epoch",
                detail: json!({ "epoch": epoch }),
            });
            self.clear_pane_state(out);
        }
        if !matches!(event, TerminalEvent::GenerationEpoch { .. }) || new_epoch.is_some() {
            out.push(HubOutput::DeliverToApp);
        }
        if let Some(epoch) = new_epoch {
            out.push(HubOutput::Epoch(epoch));
        }
        // A recovery snapshot is also an ordered boundary: native resync may
        // have discarded intervening layouts, and its seed needs the new grid.
        let grids = match &event {
            TerminalEvent::TopologyDirty { grids, .. } => grids.as_ref(),
            TerminalEvent::Snapshot {
                grids,
                authoritative: true,
                ..
            } => grids.as_ref(),
            _ => None,
        };
        for grid in grids.into_iter().flatten() {
            let PaneGrid { pane_id, size } = grid;
            if self.subscribed.contains(pane_id) {
                out.push(HubOutput::Grid {
                    pane_id: pane_id.clone(),
                    size: Some(*size),
                });
            }
        }
        let TerminalEvent::Pane(event) = event else {
            return admission;
        };
        if matches!(event, PaneEvent::History { .. }) {
            self.publish_history(event, out);
            return admission;
        }
        let pane_id = event.pane_id().to_owned();
        let was_tracked = self.panes.contains_key(&pane_id);
        let had_evicted_seed_debt = self.evicted_seed_debt.shift_remove(&pane_id);
        let requires_conservative_seed = !was_tracked && self.unknown_panes_require_seed;
        self.touch_pane(&pane_id, out);
        let health_before = self.pane_health(&pane_id);
        self.publish_to_pane(
            event,
            &pane_id,
            had_evicted_seed_debt,
            requires_conservative_seed,
            out,
        );
        self.notify_health_change(&pane_id, health_before, out);
        admission
    }

    /// A history answer is outside the ladder every other pane event goes
    /// through: it carries no generation and repairs no seed debt. An answer for
    /// an unmounted pane is dropped, because the renderer that asked is gone.
    fn publish_history(&mut self, event: PaneEvent, out: &mut Vec<HubOutput>) {
        if self.subscribed.contains(event.pane_id()) {
            out.push(HubOutput::Deliver(event));
        }
    }

    fn publish_to_pane(
        &mut self,
        event: PaneEvent,
        pane_id: &str,
        had_evicted_seed_debt: bool,
        requires_conservative_seed: bool,
        out: &mut Vec<HubOutput>,
    ) {
        if had_evicted_seed_debt || requires_conservative_seed {
            let pane = self.panes.get_mut(pane_id).expect("touched");
            let already_awaiting = pane.health.awaiting_seed;
            pane.health.awaiting_seed = true;
            self.delete_backlog(pane_id);
            // Neither incremental output nor an empty handoff can repair content
            // discarded with an evicted backlog. A resume answer is not such an
            // emptiness: it is the host verifying the screen this renderer holds.
            let cannot_repair_debt = match &event {
                PaneEvent::Output { .. } => true,
                PaneEvent::Resource(resource) => {
                    !resource.requires_seed
                        && !resource.resume_from_renderer
                        && resource.raw_tail.is_empty()
                }
                _ => false,
            };
            if cannot_repair_debt {
                if requires_conservative_seed && !had_evicted_seed_debt && !already_awaiting {
                    self.request_seed(
                        pane_id,
                        "frontend pane recovery debt outlived the metadata LRU",
                        out,
                    );
                }
                return;
            }
        }
        let mut repaint_generation = None;
        if !matches!(event, PaneEvent::SeedDiagnostic { .. }) {
            let pane = self.panes.get_mut(pane_id).expect("touched");
            let last_generation = pane.last_generation;
            match &event {
                PaneEvent::Resource(resource) => {
                    if i128::from(resource.generation) <= last_generation {
                        let same = i128::from(resource.generation) == last_generation
                            && pane
                                .backlog
                                .as_ref()
                                .and_then(PaneBacklog::latest_resource)
                                .is_some_and(|previous| same_pane_resource(previous, resource));
                        if !same {
                            self.request_conflict_reseed(pane_id, out);
                        }
                        return;
                    }
                    pane.last_generation = i128::from(resource.generation);
                    repaint_generation = Some(resource.generation);
                }
                PaneEvent::Seed { generation, .. } | PaneEvent::Output { generation, .. } => {
                    if i128::from(*generation) <= last_generation {
                        // A seed is authoritative content, not an increment:
                        // dropping one because it looks stale leaves the pane
                        // waiting for a screen that will not be sent again.
                        if matches!(event, PaneEvent::Seed { .. }) {
                            self.request_conflict_reseed(pane_id, out);
                        }
                        return;
                    }
                    pane.last_generation = i128::from(*generation);
                    repaint_generation = Some(*generation);
                }
                _ => {}
            }
            let pane = self.panes.get_mut(pane_id).expect("touched");
            match &event {
                PaneEvent::Resource(resource) if resource.requires_seed => {
                    pane.health.awaiting_seed = true;
                    self.delete_backlog(pane_id);
                }
                PaneEvent::Seed { .. } => {
                    pane.health.awaiting_seed = false;
                    pane.health.conflict_reseed_requested = false;
                }
                PaneEvent::Resource(resource) => {
                    // Recovery material replaces a locally evicted backlog. An
                    // empty reveal does not cancel a pending seed, unless it is
                    // a resume, whose emptiness means "nothing printed".
                    if resource.resume_from_renderer || !resource.raw_tail.is_empty() {
                        pane.health.awaiting_seed = false;
                        pane.health.conflict_reseed_requested = false;
                    } else if pane.health.awaiting_seed {
                        return;
                    }
                }
                _ if pane.health.awaiting_seed => return,
                _ => {}
            }
        }
        if self.subscribed.contains(pane_id) {
            if let Some(generation) = repaint_generation {
                out.push(HubOutput::Repaint {
                    pane_id: pane_id.to_owned(),
                    sequence: event.sequence(),
                    generation,
                });
            }
            out.push(HubOutput::Deliver(event));
            return;
        }
        self.buffer(pane_id, event, out);
    }

    /// Subscribes a pane's one consumer, replaying its hidden backlog. A second
    /// consumer would alias the bytes the renderer owns, so it is refused.
    pub fn subscribe_pane(
        &mut self,
        pane_id: &str,
        out: &mut Vec<HubOutput>,
    ) -> Result<(), String> {
        if self.subscribed.contains(pane_id) {
            return Err(format!(
                "terminal pane {pane_id} already has an active consumer"
            ));
        }
        self.dormant.shift_remove(pane_id);
        self.panes.entry(pane_id.to_owned()).or_default();
        self.subscribed.insert(pane_id.to_owned());
        if let Some(backlog) = self.take_backlog(pane_id) {
            out.extend(
                backlog
                    .entries
                    .into_iter()
                    .map(|(event, _)| HubOutput::Deliver(event)),
            );
        }
        self.notify_health(pane_id, out);
        Ok(())
    }

    pub fn unsubscribe_pane(&mut self, pane_id: &str, out: &mut Vec<HubOutput>) {
        if !self.subscribed.remove(pane_id) {
            return;
        }
        if self.panes.contains_key(pane_id) {
            self.retain_dormant_pane(pane_id, out);
        }
    }

    pub fn pane_health(&self, pane_id: &str) -> PaneHealth {
        self.panes
            .get(pane_id)
            .map_or_else(Default::default, |pane| pane.health)
    }

    /// Reopens the one-shot conflict latch. The pane's watchdog calls this as
    /// part of re-requesting the seed itself, so the latch bounds a storm rather
    /// than bounding recovery to one attempt. `awaiting_seed` stays closed: only
    /// an authoritative seed may clear it.
    pub fn retry_pane_seed(&mut self, pane_id: &str, out: &mut Vec<HubOutput>) {
        let Some(pane) = self.panes.get_mut(pane_id) else {
            return;
        };
        if pane.health.conflict_reseed_requested {
            let before = pane.health;
            pane.health.conflict_reseed_requested = false;
            self.notify_health_change(pane_id, before, out);
        }
    }

    /// Asks the host for one fresh screen per mounted pane, for repairs that
    /// restore ordering without restoring content.
    pub fn reseed_subscribed_panes(&mut self, reason: &str, out: &mut Vec<HubOutput>) {
        let mut mounted: Vec<String> = self.subscribed.iter().cloned().collect();
        mounted.sort();
        for pane_id in mounted {
            // The conflict latch bounds handoff storms. It must not also swallow
            // this reseed, which has its own cause.
            self.retry_pane_seed(&pane_id, out);
            self.request_seed(&pane_id, reason, out);
        }
    }

    pub fn clear_pane(&mut self, pane_id: &str, out: &mut Vec<HubOutput>) {
        self.delete_backlog(pane_id);
        self.dormant.shift_remove(pane_id);
        self.panes.remove(pane_id);
        if self.subscribed.contains(pane_id) {
            self.panes
                .insert(pane_id.to_owned(), PaneStreamState::default());
        }
        self.evicted_seed_debt.shift_remove(pane_id);
        self.notify_health(pane_id, out);
    }

    pub fn clear(&mut self, out: &mut Vec<HubOutput>) {
        self.generation_epoch = None;
        self.last_sequence = 0;
        self.clear_pane_state(out);
    }

    pub fn clear_terminal_state(&mut self, out: &mut Vec<HubOutput>) {
        self.clear_pane_state(out);
    }

    pub fn mark_rendered(
        &mut self,
        pane_id: &str,
        generation: u64,
        terminal_epoch: Option<u64>,
        out: &mut Vec<HubOutput>,
    ) {
        if terminal_epoch.is_none() || terminal_epoch != self.generation_epoch {
            return;
        }
        self.touch_pane(pane_id, out);
        let pane = self.panes.get_mut(pane_id).expect("touched");
        if generation > pane.rendered_generation {
            pane.rendered_generation = generation;
        }
    }

    pub fn visibility_checkpoint(&self, pane_id: &str) -> Option<VisibilityCheckpoint> {
        Some(VisibilityCheckpoint {
            terminal_epoch: self.generation_epoch?,
            output_generation: self
                .panes
                .get(pane_id)
                .map_or(0, |pane| pane.rendered_generation),
        })
    }

    pub fn generation_epoch(&self) -> Option<u64> {
        self.generation_epoch
    }

    pub fn last_sequence(&self) -> u64 {
        self.last_sequence
    }

    pub fn retained_pane_count(&self) -> usize {
        self.backlog_count
    }

    pub fn retained_byte_length(&self) -> usize {
        self.backlog_bytes
    }

    pub fn tracked_pane_count(&self) -> usize {
        self.panes.len()
    }

    /// Marks a pane as owing a seed, for a consumer that could not apply what it
    /// was given and so can no longer prove what reached its screen.
    pub fn require_seed(&mut self, pane_id: &str, reason: &str, out: &mut Vec<HubOutput>) {
        if !self.panes.contains_key(pane_id) {
            self.touch_pane(pane_id, out);
        }
        let before = self.pane_health(pane_id);
        let pane = self.panes.get_mut(pane_id).expect("touched");
        let already_awaiting = pane.health.awaiting_seed;
        pane.health.awaiting_seed = true;
        self.delete_backlog(pane_id);
        if !already_awaiting {
            self.request_seed(pane_id, reason, out);
        }
        self.notify_health_change(pane_id, before, out);
    }

    fn clear_pane_state(&mut self, out: &mut Vec<HubOutput>) {
        let mut mounted: Vec<String> = self.subscribed.iter().cloned().collect();
        mounted.sort();
        for pane_id in &mounted {
            out.push(HubOutput::Grid {
                pane_id: pane_id.clone(),
                size: None,
            });
        }
        self.dormant.clear();
        self.panes.clear();
        for pane_id in &mounted {
            self.panes
                .insert(pane_id.clone(), PaneStreamState::default());
        }
        self.backlog_bytes = 0;
        self.backlog_count = 0;
        self.evicted_seed_debt.clear();
        self.unknown_panes_require_seed = false;
        // A pane whose watchdog is armed on the debt just discarded has to hear
        // about it, or it keeps re-requesting a seed nobody owes it any more.
        for pane_id in &mounted {
            self.notify_health(pane_id, out);
        }
    }

    fn buffer(&mut self, pane_id: &str, event: PaneEvent, out: &mut Vec<HubOutput>) {
        let mut backlog = self.take_backlog(pane_id).unwrap_or_default();
        match &event {
            PaneEvent::Seed { data, .. } => {
                let bytes = data.len();
                backlog.replace(event, bytes);
            }
            PaneEvent::Resource(resource) => {
                let bytes = resource.raw_tail.len();
                let reason_bytes = resource.recovery_reason.len();
                if bytes > 0 {
                    backlog.replace(event, bytes + reason_bytes);
                } else {
                    backlog.remove_kind(|entry| matches!(entry, PaneEvent::Resource(_)));
                    backlog.append(event, reason_bytes);
                }
            }
            PaneEvent::SeedDiagnostic { message, .. } => {
                let bytes = message.len();
                backlog.remove_kind(|entry| matches!(entry, PaneEvent::SeedDiagnostic { .. }));
                backlog.append(event, bytes);
            }
            PaneEvent::Output { data, .. } => {
                let bytes = data.len();
                backlog.append(event, bytes);
            }
            PaneEvent::History { .. } => return,
        }
        let HubLimits {
            max_pane_bytes,
            max_pane_events,
            ..
        } = self.limits;
        if backlog.byte_length > max_pane_bytes || backlog.entries.len() > max_pane_events {
            let reason = if backlog.byte_length > max_pane_bytes {
                let limit = if max_pane_bytes == DEFAULT_MAX_PANE_BYTES {
                    "8 MiB".to_owned()
                } else {
                    format!("{max_pane_bytes} bytes")
                };
                format!("frontend hidden recovery buffer exceeded {limit}")
            } else {
                format!(
                    "frontend hidden recovery record capacity exceeded {max_pane_events} events"
                )
            };
            self.require_seed(pane_id, &reason, out);
            return;
        }
        self.set_backlog(pane_id, backlog);
        self.enforce_backlog_limits(out);
    }

    fn set_backlog(&mut self, pane_id: &str, backlog: PaneBacklog) {
        self.backlog_count += 1;
        self.backlog_bytes += backlog.byte_length;
        self.panes.get_mut(pane_id).expect("tracked").backlog = Some(backlog);
    }

    fn take_backlog(&mut self, pane_id: &str) -> Option<PaneBacklog> {
        let backlog = self.panes.get_mut(pane_id)?.backlog.take()?;
        self.backlog_bytes -= backlog.byte_length;
        self.backlog_count -= 1;
        Some(backlog)
    }

    fn delete_backlog(&mut self, pane_id: &str) {
        self.take_backlog(pane_id);
    }

    fn enforce_backlog_limits(&mut self, out: &mut Vec<HubOutput>) {
        while self.backlog_count > self.limits.max_buffered_panes
            || self.backlog_bytes > self.limits.max_total_bytes
        {
            let Some(oldest) = self.oldest_buffered_pane() else {
                break;
            };
            let reason = if self.backlog_bytes > self.limits.max_total_bytes {
                "frontend aggregate hidden recovery budget was exceeded"
            } else {
                "frontend hidden-pane LRU capacity was exceeded"
            };
            self.require_seed(&oldest, reason, out);
        }
    }

    fn touch_pane(&mut self, pane_id: &str, out: &mut Vec<HubOutput>) {
        if self.subscribed.contains(pane_id) {
            self.panes.entry(pane_id.to_owned()).or_default();
            return;
        }
        self.panes.entry(pane_id.to_owned()).or_default();
        self.retain_dormant_pane(pane_id, out);
    }

    fn retain_dormant_pane(&mut self, pane_id: &str, out: &mut Vec<HubOutput>) {
        self.dormant.shift_remove(pane_id);
        self.dormant.insert(pane_id.to_owned());
        self.enforce_dormant_pane_limit(out);
    }

    fn enforce_dormant_pane_limit(&mut self, out: &mut Vec<HubOutput>) {
        while self.dormant.len() > self.limits.max_tracked_panes {
            let Some(oldest) = self.dormant.shift_remove_index(0) else {
                break;
            };
            let already_awaiting = self.pane_health(&oldest).awaiting_seed;
            self.delete_backlog(&oldest);
            self.panes.remove(&oldest);
            self.remember_evicted_seed_debt(oldest.clone());
            if !already_awaiting {
                self.request_seed(
                    &oldest,
                    "frontend pane metadata LRU capacity was exceeded",
                    out,
                );
            }
        }
    }

    fn oldest_buffered_pane(&self) -> Option<String> {
        self.dormant
            .iter()
            .find(|pane_id| {
                self.panes
                    .get(*pane_id)
                    .is_some_and(|pane| pane.backlog.is_some())
            })
            .cloned()
    }

    fn remember_evicted_seed_debt(&mut self, pane_id: String) {
        self.evicted_seed_debt.shift_remove(&pane_id);
        self.evicted_seed_debt.insert(pane_id);
        while self.evicted_seed_debt.len() > self.limits.max_tracked_panes {
            self.evicted_seed_debt.shift_remove_index(0);
            // One bounded sentinel suffices once the exact tombstone is gone:
            // any later untracked pane must establish itself with a seed.
            self.unknown_panes_require_seed = true;
        }
    }

    /// Where the wire's ordering is read, not where it is enforced. The native
    /// link owns sequence integrity, so a hole reaching this far is this process
    /// dropping a decoded frame: admit it, and repair the screens at risk.
    fn admit_sequence(
        &mut self,
        event: &TerminalEvent,
        now: Instant,
        out: &mut Vec<HubOutput>,
    ) -> Admission {
        let sequence = event.sequence();
        if sequence == 0 {
            if let TerminalEvent::GenerationEpoch { epoch, .. } = event
                && Some(*epoch) != self.generation_epoch
            {
                self.last_sequence = 0;
            }
            return Admission::Local;
        }
        if matches!(
            event,
            TerminalEvent::Snapshot {
                authoritative: true,
                ..
            }
        ) {
            self.last_sequence = sequence;
            return Admission::Accepted;
        }
        if sequence <= self.last_sequence {
            return Admission::Stale;
        }
        let expected = self.last_sequence + 1;
        self.last_sequence = sequence;
        if sequence != expected {
            self.repair_sequence_jump(expected, sequence, now, out);
        }
        Admission::Accepted
    }

    fn repair_sequence_jump(
        &mut self,
        expected: u64,
        received: u64,
        now: Instant,
        out: &mut Vec<HubOutput>,
    ) {
        if self
            .last_sequence_repair_at
            .is_some_and(|at| now.saturating_duration_since(at) < SEQUENCE_REPAIR_DEBOUNCE)
        {
            return;
        }
        self.last_sequence_repair_at = Some(now);
        out.push(HubOutput::Incident {
            kind: "link.eventGap",
            detail: json!({ "expected": expected, "received": received }),
        });
        self.reseed_subscribed_panes("event sequence jumped (decode drop?)", out);
    }

    fn request_conflict_reseed(&mut self, pane_id: &str, out: &mut Vec<HubOutput>) {
        let pane = self.panes.get_mut(pane_id).expect("touched");
        if pane.health.conflict_reseed_requested {
            return;
        }
        pane.health.conflict_reseed_requested = true;
        self.request_seed(
            pane_id,
            "stale or conflicting terminal visibility handoff",
            out,
        );
    }

    fn notify_health_change(&self, pane_id: &str, before: PaneHealth, out: &mut Vec<HubOutput>) {
        if self.subscribed.contains(pane_id) && before != self.pane_health(pane_id) {
            self.notify_health(pane_id, out);
        }
    }

    fn notify_health(&self, pane_id: &str, out: &mut Vec<HubOutput>) {
        if self.subscribed.contains(pane_id) {
            out.push(HubOutput::Health {
                pane_id: pane_id.to_owned(),
                health: self.pane_health(pane_id),
            });
        }
    }

    fn request_seed(&self, pane_id: &str, reason: &str, out: &mut Vec<HubOutput>) {
        out.push(HubOutput::SeedRequired {
            pane_id: pane_id.to_owned(),
            reason: reason.to_owned(),
        });
    }
}

fn same_pane_resource(left: &PaneResource, right: &PaneResource) -> bool {
    if right.recovery_reason.len() + right.raw_tail.len() > MAX_EXACT_RESOURCE_IDENTITY_BYTES {
        return false;
    }
    left.state == right.state
        && left.requires_seed == right.requires_seed
        && left.resume_from_renderer == right.resume_from_renderer
        && left.generation == right.generation
        && left.snapshot_generation == right.snapshot_generation
        && left.tail_through_generation == right.tail_through_generation
        && left.recovery_reason == right.recovery_reason
        && left.raw_tail == right.raw_tail
}

#[cfg(test)]
#[path = "hub_tests.rs"]
mod tests;
