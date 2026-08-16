#[cfg(test)]
use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};

#[path = "replay_lru.rs"]
mod replay_lru;
use replay_lru::Lru;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BufferedOutput {
    pub sequence: u64,
    pub bytes: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplayBatch {
    pub seed: Vec<u8>,
    pub replay: Vec<BufferedOutput>,
}

#[derive(Debug)]
pub struct ScreenSeeder {
    output: Vec<BufferedOutput>,
    buffered_bytes: usize,
    max_buffered_bytes: usize,
    overflowed: bool,
}

impl Default for ScreenSeeder {
    fn default() -> Self {
        Self::with_limit(4 * 1024 * 1024)
    }
}

impl ScreenSeeder {
    pub fn with_limit(max_buffered_bytes: usize) -> Self {
        Self {
            output: Vec::new(),
            buffered_bytes: 0,
            max_buffered_bytes,
            overflowed: false,
        }
    }

    /// Returns false once output-during-capture exceeded the bounded replay
    /// budget. Callers must discard the seed and perform a fresh resnapshot.
    pub fn buffer(&mut self, sequence: u64, bytes: Vec<u8>) -> bool {
        if self.overflowed
            || self.buffered_bytes.saturating_add(bytes.len()) > self.max_buffered_bytes
        {
            self.output.clear();
            self.buffered_bytes = 0;
            self.overflowed = true;
            return false;
        }
        self.buffered_bytes += bytes.len();
        self.output.push(BufferedOutput { sequence, bytes });
        true
    }

    /// Completes a screen seed captured at `boundary_sequence`.
    ///
    /// Output at or before the boundary is represented by the seed. Later
    /// output is replayed exactly once and in its original order.
    pub fn complete(self, seed: Vec<u8>, boundary_sequence: u64) -> ReplayBatch {
        ReplayBatch {
            seed,
            replay: self
                .output
                .into_iter()
                .filter(|output| output.sequence > boundary_sequence)
                .collect(),
        }
    }

    pub fn requires_resnapshot(&self) -> bool {
        self.overflowed
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaneResourceState {
    Visible,
    HiddenBuffered,
    Released,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaneResource {
    pub state: PaneResourceState,
    pub serialized_snapshot: Vec<u8>,
    pub raw_tail: Vec<u8>,
    pub generation: u64,
    pub snapshot_generation: u64,
    pub tail_through_generation: u64,
    pub requires_seed: bool,
    pub recovery_reason: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputDisposition {
    Visible,
    Hidden,
    Released,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VisibilityCheckpoint {
    pub epoch: u64,
    pub generation: u64,
}

/// Deterministic operation counts owned by the test-only Phase 14 observer.
#[cfg(test)]
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct PaneResourceMeasurements {
    full_accounting_scans: usize,
    accounting_entries_visited: usize,
    lru_retain_operations: usize,
    lru_entries_visited: usize,
    lru_pops: usize,
    append_operations: usize,
    appended_bytes: usize,
    evictions: usize,
}

#[cfg(test)]
thread_local! {
    static PANE_RESOURCE_MEASUREMENTS: RefCell<Option<PaneResourceMeasurements>> = const { RefCell::new(None) };
}

#[cfg(test)]
fn begin_pane_resource_measurement() {
    PANE_RESOURCE_MEASUREMENTS.with(|value| {
        *value.borrow_mut() = Some(PaneResourceMeasurements::default());
    });
}

#[cfg(test)]
fn pane_resource_measurement_snapshot() -> PaneResourceMeasurements {
    PANE_RESOURCE_MEASUREMENTS.with(|value| {
        value
            .borrow()
            .clone()
            .expect("pane-resource measurement was not started")
    })
}

#[cfg(not(test))]
macro_rules! record_pane_resource_measurement {
    ($update:expr) => {};
}

#[cfg(test)]
macro_rules! record_pane_resource_measurement {
    ($update:expr) => {
        PANE_RESOURCE_MEASUREMENTS.with(|value| {
            if let Some(measurements) = value.borrow_mut().as_mut() {
                $update(measurements);
            }
        });
    };
}

#[derive(Debug, Clone, Copy, Default)]
struct AccountedState {
    bytes: usize,
    journal_bytes: usize,
    retained: bool,
}

/// Bounded host-side recovery material for panes whose renderer is hidden or
/// disposed. Terminal bytes remain opaque and are never decoded here.
#[derive(Debug)]
pub struct PaneResourceStore {
    resources: HashMap<String, PaneResource>,
    output_journals: HashMap<String, VecDeque<BufferedOutput>>,
    output_journal_bytes: HashMap<String, usize>,
    handoff_checkpoints: HashMap<String, VisibilityCheckpoint>,
    retained_lru: Lru,
    byte_lru: Lru,
    retained_bytes: usize,
    retained_panes: usize,
    journal_bytes: usize,
    max_hidden_panes: usize,
    max_resource_bytes: usize,
    max_total_bytes: usize,
}

impl PaneResourceStore {
    pub fn new(max_hidden_panes: usize, max_tail_bytes: usize) -> Self {
        Self::with_total_limit(
            max_hidden_panes,
            max_tail_bytes,
            max_hidden_panes.saturating_mul(max_tail_bytes),
        )
    }

    pub fn with_total_limit(
        max_hidden_panes: usize,
        max_resource_bytes: usize,
        max_total_bytes: usize,
    ) -> Self {
        Self {
            resources: HashMap::new(),
            output_journals: HashMap::new(),
            output_journal_bytes: HashMap::new(),
            handoff_checkpoints: HashMap::new(),
            retained_lru: Lru::default(),
            byte_lru: Lru::default(),
            retained_bytes: 0,
            retained_panes: 0,
            journal_bytes: 0,
            max_hidden_panes,
            max_resource_bytes,
            max_total_bytes,
        }
    }

    pub fn ensure(&mut self, pane_id: &str, visible: bool, generation: u64) {
        if self.resources.contains_key(pane_id) {
            return;
        }
        let before = self.accounted_state(pane_id);
        self.resources.insert(
            pane_id.to_owned(),
            PaneResource {
                state: if visible {
                    PaneResourceState::Visible
                } else {
                    PaneResourceState::HiddenBuffered
                },
                serialized_snapshot: Vec::new(),
                raw_tail: Vec::new(),
                generation,
                snapshot_generation: generation,
                tail_through_generation: generation,
                requires_seed: false,
                recovery_reason: String::new(),
            },
        );
        self.refresh_accounting(pane_id, before);
        self.enforce_limits();
    }

    /// Compatibility transition used by non-checkpointed callers. Production
    /// renderer handoff uses `hide_with_checkpoint`.
    pub fn set_visible(&mut self, pane_id: &str, visible: bool, generation: u64) {
        self.ensure(pane_id, visible, generation);
        let before = self.accounted_state(pane_id);
        if visible {
            if let Some(resource) = self.resources.get_mut(pane_id) {
                resource.state = PaneResourceState::Visible;
                resource.generation = generation;
                resource.snapshot_generation = generation;
                resource.tail_through_generation = generation;
            }
            self.handoff_checkpoints.remove(pane_id);
            self.output_journals.remove(pane_id);
            self.output_journal_bytes.remove(pane_id);
        } else if let Some(resource) = self.resources.get_mut(pane_id) {
            if resource.state != PaneResourceState::Released {
                resource.state = PaneResourceState::HiddenBuffered;
                resource.requires_seed = false;
                resource.recovery_reason.clear();
            }
            resource.generation = generation;
        }
        self.refresh_accounting(pane_id, before);
        self.enforce_limits();
    }

    /// Atomically transfers renderer ownership to the host. The renderer
    /// snapshot includes output through `checkpoint.generation`; output
    /// observed by the host after that cutoff is retained exactly once.
    pub fn hide_with_checkpoint(
        &mut self,
        pane_id: &str,
        snapshot: Vec<u8>,
        checkpoint: VisibilityCheckpoint,
        generation: u64,
    ) -> Result<PaneResource, String> {
        if checkpoint.epoch == 0 {
            return Err("terminal visibility checkpoint epoch must be non-zero".into());
        }
        self.ensure(pane_id, true, generation);
        if self.handoff_checkpoints.get(pane_id) == Some(&checkpoint) {
            return self
                .resources
                .get(pane_id)
                .cloned()
                .ok_or_else(|| "pane resource missing".into());
        }
        let state = self.resources.get(pane_id).map(|resource| resource.state);
        if state != Some(PaneResourceState::Visible) {
            return Err("pane renderer ownership is already held by the host".into());
        }
        let observed_generation = self
            .resources
            .get(pane_id)
            .map(|resource| resource.generation)
            .unwrap_or_default();
        if checkpoint.generation > observed_generation {
            return Err(
                "renderer visibility cutoff is newer than host-observed pane output".into(),
            );
        }

        let before = self.accounted_state(pane_id);
        let journal_bytes = self
            .output_journal_bytes
            .remove(pane_id)
            .unwrap_or_default();
        let mut tail = Vec::with_capacity(journal_bytes);
        let mut tail_through_generation = checkpoint.generation;
        if let Some(journal) = self.output_journals.remove(pane_id) {
            for output in journal {
                if output.sequence > checkpoint.generation {
                    tail.extend_from_slice(&output.bytes);
                    tail_through_generation = output.sequence;
                }
            }
        }
        let exceeds_resource = snapshot.len().saturating_add(tail.len()) > self.max_resource_bytes;
        let resource = self.resources.get_mut(pane_id).expect("resource ensured");
        resource.generation = tail_through_generation.max(generation);
        resource.snapshot_generation = checkpoint.generation;
        resource.tail_through_generation = tail_through_generation;
        if resource.requires_seed {
            release(resource, "visible output handoff journal was not retained");
        } else if exceeds_resource {
            release(resource, "renderer handoff exceeded the hidden-pane budget");
        } else {
            resource.state = PaneResourceState::HiddenBuffered;
            resource.serialized_snapshot = snapshot;
            resource.raw_tail = tail;
            resource.requires_seed = false;
            resource.recovery_reason.clear();
        }
        self.handoff_checkpoints
            .insert(pane_id.to_owned(), checkpoint);
        self.refresh_accounting(pane_id, before);
        self.enforce_limits();
        Ok(self
            .resources
            .get(pane_id)
            .expect("resource exists")
            .clone())
    }

    /// Makes a pane renderer-owned and consumes host recovery bytes once.
    pub fn reveal(&mut self, pane_id: &str, generation: u64) -> Option<PaneResource> {
        let before = self.accounted_state(pane_id);
        let resource = self.resources.get_mut(pane_id)?;
        if resource.state == PaneResourceState::Visible {
            resource.generation = generation;
            return Some(PaneResource {
                state: PaneResourceState::Visible,
                serialized_snapshot: Vec::new(),
                raw_tail: Vec::new(),
                generation,
                snapshot_generation: generation,
                tail_through_generation: generation,
                requires_seed: resource.requires_seed,
                recovery_reason: resource.recovery_reason.clone(),
            });
        }
        let recovery = PaneResource {
            state: resource.state,
            serialized_snapshot: std::mem::take(&mut resource.serialized_snapshot),
            raw_tail: std::mem::take(&mut resource.raw_tail),
            generation: resource.generation,
            snapshot_generation: resource.snapshot_generation,
            tail_through_generation: resource.tail_through_generation,
            requires_seed: resource.requires_seed,
            recovery_reason: resource.recovery_reason.clone(),
        };
        resource.state = PaneResourceState::Visible;
        resource.generation = generation;
        self.output_journals.remove(pane_id);
        self.output_journal_bytes.remove(pane_id);
        self.handoff_checkpoints.remove(pane_id);
        self.refresh_accounting(pane_id, before);
        Some(recovery)
    }

    pub fn snapshot(&mut self, pane_id: &str, snapshot: Vec<u8>, generation: u64) {
        self.ensure(pane_id, false, generation);
        let before = self.accounted_state(pane_id);
        self.output_journals.remove(pane_id);
        self.output_journal_bytes.remove(pane_id);
        let resource = self.resources.get_mut(pane_id).expect("resource ensured");
        if snapshot.len() > self.max_resource_bytes {
            release(
                resource,
                "serialized snapshot exceeded the hidden-pane budget",
            );
        } else {
            resource.serialized_snapshot = snapshot;
            resource.raw_tail.clear();
            resource.snapshot_generation = generation;
            resource.tail_through_generation = generation;
            resource.requires_seed = false;
            resource.recovery_reason.clear();
        }
        resource.generation = generation;
        self.refresh_accounting(pane_id, before);
        self.enforce_limits();
    }

    /// Records output before deciding whether to emit it. This mutex-protected
    /// operation is the visibility handoff boundary.
    pub fn record_output(
        &mut self,
        pane_id: &str,
        bytes: &[u8],
        generation: u64,
    ) -> OutputDisposition {
        self.ensure(pane_id, false, generation);
        let state = self.resources.get(pane_id).expect("resource ensured").state;
        match state {
            PaneResourceState::Visible => {
                let before = self.accounted_state(pane_id);
                self.output_journals
                    .entry(pane_id.to_owned())
                    .or_default()
                    .push_back(BufferedOutput {
                        sequence: generation,
                        bytes: bytes.to_vec(),
                    });
                *self
                    .output_journal_bytes
                    .entry(pane_id.to_owned())
                    .or_default() += bytes.len();
                if let Some(resource) = self.resources.get_mut(pane_id) {
                    resource.generation = generation;
                    resource.tail_through_generation = generation;
                }
                self.refresh_accounting(pane_id, before);
                self.enforce_limits();
                OutputDisposition::Visible
            }
            PaneResourceState::HiddenBuffered => {
                self.append_hidden(pane_id, bytes, generation);
                OutputDisposition::Hidden
            }
            PaneResourceState::Released => {
                if let Some(resource) = self.resources.get_mut(pane_id) {
                    resource.generation = generation;
                }
                OutputDisposition::Released
            }
        }
    }

    pub fn append(&mut self, pane_id: &str, bytes: &[u8], generation: u64) {
        self.ensure(pane_id, false, generation);
        if self
            .resources
            .get(pane_id)
            .is_some_and(|resource| resource.state == PaneResourceState::HiddenBuffered)
        {
            self.append_hidden(pane_id, bytes, generation);
        } else if let Some(resource) = self.resources.get_mut(pane_id) {
            resource.generation = generation;
        }
    }

    pub fn append_if_hidden(&mut self, pane_id: &str, bytes: &[u8], generation: u64) {
        self.append(pane_id, bytes, generation);
    }

    fn append_hidden(&mut self, pane_id: &str, bytes: &[u8], generation: u64) {
        record_pane_resource_measurement!(|measurements: &mut PaneResourceMeasurements| {
            measurements.append_operations += 1;
            measurements.appended_bytes = measurements.appended_bytes.saturating_add(bytes.len());
        });
        let before = self.accounted_state(pane_id);
        let resource = self.resources.get_mut(pane_id).expect("resource ensured");
        if resource
            .serialized_snapshot
            .len()
            .saturating_add(resource.raw_tail.len())
            .saturating_add(bytes.len())
            > self.max_resource_bytes
        {
            release(resource, "raw output tail exceeded the hidden-pane budget");
        } else {
            resource.raw_tail.extend_from_slice(bytes);
            resource.tail_through_generation = generation;
        }
        resource.generation = generation;
        self.refresh_accounting(pane_id, before);
        self.enforce_limits();
    }

    pub fn get(&self, pane_id: &str) -> Option<&PaneResource> {
        self.resources.get(pane_id)
    }

    pub fn is_hidden(&self, pane_id: &str) -> bool {
        self.resources
            .get(pane_id)
            .is_none_or(|resource| resource.state != PaneResourceState::Visible)
    }

    pub fn retained_bytes(&self) -> usize {
        self.retained_bytes
    }

    pub fn journal_bytes(&self) -> usize {
        self.journal_bytes
    }

    pub fn take_recovery(&mut self, pane_id: &str) -> Option<PaneResource> {
        let before = self.accounted_state(pane_id);
        let resource = self.resources.get_mut(pane_id)?;
        let recovery = PaneResource {
            state: resource.state,
            serialized_snapshot: std::mem::take(&mut resource.serialized_snapshot),
            raw_tail: std::mem::take(&mut resource.raw_tail),
            generation: resource.generation,
            snapshot_generation: resource.snapshot_generation,
            tail_through_generation: resource.tail_through_generation,
            requires_seed: resource.requires_seed,
            recovery_reason: resource.recovery_reason.clone(),
        };
        self.refresh_accounting(pane_id, before);
        Some(recovery)
    }

    pub fn remove(&mut self, pane_id: &str) {
        let before = self.accounted_state(pane_id);
        self.output_journals.remove(pane_id);
        self.output_journal_bytes.remove(pane_id);
        self.handoff_checkpoints.remove(pane_id);
        self.resources.remove(pane_id);
        self.refresh_accounting(pane_id, before);
    }

    fn pop_eviction_candidate(&mut self, over_bytes: bool) -> Option<String> {
        let pane_id = if over_bytes {
            self.byte_lru.pop_oldest()
        } else {
            self.retained_lru.pop_oldest()
        }?;
        record_pane_resource_measurement!(|measurements: &mut PaneResourceMeasurements| {
            measurements.lru_pops += 1;
        });
        Some(pane_id)
    }

    fn accounted_state(&self, pane_id: &str) -> AccountedState {
        let resource = self.resources.get(pane_id);
        let resource_bytes = resource.map_or(0, |resource| {
            resource
                .serialized_snapshot
                .len()
                .saturating_add(resource.raw_tail.len())
        });
        let journal_bytes = self
            .output_journal_bytes
            .get(pane_id)
            .copied()
            .unwrap_or_default();
        let has_journal = self
            .output_journals
            .get(pane_id)
            .is_some_and(|journal| !journal.is_empty());
        AccountedState {
            bytes: resource_bytes.saturating_add(journal_bytes),
            journal_bytes,
            retained: resource.is_some_and(|resource| {
                resource.state == PaneResourceState::HiddenBuffered
                    || resource_bytes != 0
                    || has_journal
            }),
        }
    }

    fn refresh_accounting(&mut self, pane_id: &str, before: AccountedState) {
        let after = self.accounted_state(pane_id);
        self.retained_bytes = self
            .retained_bytes
            .checked_sub(before.bytes)
            .expect("pane retained byte accounting underflow")
            .saturating_add(after.bytes);
        self.journal_bytes = self
            .journal_bytes
            .checked_sub(before.journal_bytes)
            .expect("pane journal byte accounting underflow")
            .saturating_add(after.journal_bytes);
        match (before.retained, after.retained) {
            (false, true) => self.retained_panes = self.retained_panes.saturating_add(1),
            (true, false) => {
                self.retained_panes = self
                    .retained_panes
                    .checked_sub(1)
                    .expect("pane retained count accounting underflow");
            }
            _ => {}
        }
        if after.retained {
            self.retained_lru.touch(pane_id);
        } else {
            self.retained_lru.detach(pane_id);
        }
        if after.bytes != 0 {
            self.byte_lru.touch(pane_id);
        } else {
            self.byte_lru.detach(pane_id);
        }
    }

    fn retained_panes(&self) -> usize {
        self.retained_panes
    }

    pub fn retained_panes_for_measurement(&self) -> usize {
        self.retained_panes()
    }

    fn enforce_limits(&mut self) {
        while self.retained_panes() > self.max_hidden_panes
            || self.retained_bytes() > self.max_total_bytes
        {
            let over_bytes = self.retained_bytes() > self.max_total_bytes;
            let Some(pane_id) = self.pop_eviction_candidate(over_bytes) else {
                break;
            };
            let before = self.accounted_state(&pane_id);
            self.output_journals.remove(&pane_id);
            self.output_journal_bytes.remove(&pane_id);
            if let Some(resource) = self.resources.get_mut(&pane_id) {
                record_pane_resource_measurement!(|measurements: &mut PaneResourceMeasurements| {
                    measurements.evictions += 1;
                });
                let reason = if over_bytes {
                    "global pane-resource byte budget was exceeded"
                } else {
                    "global pane-resource LRU capacity was exceeded"
                };
                if resource.state == PaneResourceState::Visible {
                    resource.serialized_snapshot.clear();
                    resource.raw_tail.clear();
                    resource.requires_seed = true;
                    resource.recovery_reason = reason.into();
                } else {
                    release(resource, reason);
                }
            }
            self.refresh_accounting(&pane_id, before);
        }
    }
}

fn release(resource: &mut PaneResource, reason: &str) {
    resource.state = PaneResourceState::Released;
    resource.serialized_snapshot.clear();
    resource.raw_tail.clear();
    resource.requires_seed = true;
    resource.recovery_reason = reason.into();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn excludes_seeded_output_and_replays_output_during_seed() {
        let mut seeder = ScreenSeeder::default();
        assert!(seeder.buffer(10, b"already in seed".to_vec()));
        assert!(seeder.buffer(12, b"during capture".to_vec()));
        assert!(seeder.buffer(13, b"after capture".to_vec()));

        let batch = seeder.complete(b"screen at 10".to_vec(), 10);
        assert_eq!(batch.seed, b"screen at 10");
        assert_eq!(
            batch
                .replay
                .iter()
                .map(|output| output.bytes.as_slice())
                .collect::<Vec<_>>(),
            [b"during capture".as_slice(), b"after capture".as_slice()]
        );
        assert_eq!(batch.replay[0].sequence, 12);
    }

    #[test]
    fn renderer_hide_handoff_preserves_only_output_after_cutoff_and_is_idempotent() {
        let mut store = PaneResourceStore::with_total_limit(32, 1024, 4096);
        store.ensure("%1", true, 1);
        assert_eq!(
            store.record_output("%1", b"included", 10),
            OutputDisposition::Visible
        );
        assert_eq!(
            store.record_output("%1", b"between", 11),
            OutputDisposition::Visible
        );
        let checkpoint = VisibilityCheckpoint {
            epoch: 7,
            generation: 10,
        };
        let hidden = store
            .hide_with_checkpoint("%1", b"snapshot@10".to_vec(), checkpoint, 12)
            .unwrap();
        assert_eq!(hidden.raw_tail, b"between");
        assert_eq!(hidden.snapshot_generation, 10);
        assert_eq!(hidden.tail_through_generation, 11);
        assert_eq!(
            store.record_output("%1", b"after", 13),
            OutputDisposition::Hidden
        );
        let repeated = store
            .hide_with_checkpoint("%1", b"must-not-replace".to_vec(), checkpoint, 14)
            .unwrap();
        assert_eq!(repeated.serialized_snapshot, b"snapshot@10");
        assert_eq!(repeated.raw_tail, b"betweenafter");
        assert_eq!(repeated.snapshot_generation, 10);
        assert_eq!(repeated.tail_through_generation, 13);
        let recovery = store.reveal("%1", 15).unwrap();
        assert_eq!(recovery.serialized_snapshot, b"snapshot@10");
        assert_eq!(recovery.raw_tail, b"betweenafter");
        assert_eq!(recovery.snapshot_generation, 10);
        assert_eq!(recovery.tail_through_generation, 13);
        assert!(store.reveal("%1", 16).unwrap().raw_tail.is_empty());
    }

    #[test]
    fn rapid_hide_reveal_has_an_exact_deferred_output_discard_boundary() {
        let mut store = PaneResourceStore::with_total_limit(32, 1024, 4096);
        store.ensure("%1", true, 1);
        for (generation, bytes) in [(20, b"A"), (21, b"B"), (22, b"C")] {
            assert_eq!(
                store.record_output("%1", bytes, generation),
                OutputDisposition::Visible
            );
        }
        let recovery = store
            .hide_with_checkpoint(
                "%1",
                b"screen+A".to_vec(),
                VisibilityCheckpoint {
                    epoch: 3,
                    generation: 20,
                },
                22,
            )
            .unwrap();
        assert_eq!(
            store.record_output("%1", b"D", 23),
            OutputDisposition::Hidden
        );
        let recovery = store.reveal("%1", 24).unwrap_or(recovery);
        assert_eq!(recovery.serialized_snapshot, b"screen+A");
        assert_eq!(recovery.raw_tail, b"BCD");
        assert_eq!(recovery.snapshot_generation, 20);
        assert_eq!(recovery.tail_through_generation, 23);

        let deferred = [(21, b"B"), (22, b"C"), (23, b"D"), (25, b"E")];
        let remaining: Vec<_> = deferred
            .into_iter()
            .filter(|(generation, _)| *generation > recovery.tail_through_generation)
            .flat_map(|(_, bytes)| bytes)
            .copied()
            .collect();
        assert_eq!(remaining, b"E");
        let combined = [recovery.serialized_snapshot, recovery.raw_tail, remaining].concat();
        assert_eq!(combined, b"screen+ABCDE");
    }

    #[test]
    fn renderer_handoff_rejects_a_cutoff_not_observed_for_that_pane() {
        let mut store = PaneResourceStore::new(32, 1024);
        store.ensure("%1", true, 5);
        let error = store
            .hide_with_checkpoint(
                "%1",
                b"screen".to_vec(),
                VisibilityCheckpoint {
                    epoch: 1,
                    generation: 6,
                },
                5,
            )
            .unwrap_err();
        assert!(error.contains("newer than host-observed pane output"));
        assert_eq!(store.get("%1").unwrap().state, PaneResourceState::Visible);
    }

    #[test]
    fn global_budget_evicts_visible_recovery_without_a_hidden_lru() {
        let mut store = PaneResourceStore::with_total_limit(32, 1024, 8 * 1024);
        for index in 0..100_u64 {
            let pane = format!("%{index}");
            store.ensure(&pane, true, index);
            store.snapshot(&pane, vec![b'x'; 1024], index);
        }
        assert!(store.retained_bytes() <= 8 * 1024);
        assert!(store.resources.values().any(|resource| {
            resource.state == PaneResourceState::Visible && resource.requires_seed
        }));
    }

    #[test]
    fn capture_buffer_overflow_requires_resnapshot() {
        let mut seeder = ScreenSeeder::with_limit(3);
        assert!(seeder.buffer(1, vec![1, 2]));
        assert!(!seeder.buffer(2, vec![3, 4]));
        assert!(seeder.requires_resnapshot());
        assert!(seeder.complete(Vec::new(), 0).replay.is_empty());
    }

    #[test]
    fn hidden_resource_tails_and_lru_are_bounded() {
        let mut store = PaneResourceStore::new(1, 4);
        store.set_visible("%1", false, 1);
        store.append("%1", b"abcdef", 2);
        assert!(store.get("%1").unwrap().requires_seed);
        store.set_visible("%1", true, 3);
        assert!(store.get("%1").unwrap().requires_seed);
        store.set_visible("%1", false, 4);
        store.set_visible("%2", false, 3);
        assert_eq!(store.get("%1").unwrap().state, PaneResourceState::Released);
        assert!(store.get("%1").unwrap().requires_seed);
        assert_eq!(
            store.get("%2").unwrap().state,
            PaneResourceState::HiddenBuffered
        );
    }

    #[test]
    fn oversized_snapshot_and_thirty_third_hidden_pane_require_seed() {
        let mut store = PaneResourceStore::new(32, 4 * 1024 * 1024);
        store.set_visible("%0", false, 1);
        store.snapshot("%0", vec![0; 4 * 1024 * 1024 + 1], 2);
        assert!(store.get("%0").unwrap().requires_seed);
        for index in 1..=33 {
            store.set_visible(&format!("%{index}"), false, index + 2);
        }
        assert!(store.get("%1").unwrap().requires_seed);
        assert_eq!(
            store.get("%2").unwrap().state,
            PaneResourceState::HiddenBuffered
        );
        store.set_visible("%1", true, 40);
        assert_eq!(store.get("%1").unwrap().state, PaneResourceState::Visible);
        assert!(store.get("%1").unwrap().requires_seed);

        store.set_visible("%33", false, 41);
        store.append("%33", &vec![b'x'; 4 * 1024 * 1024 + 1], 42);
        assert_eq!(store.get("%33").unwrap().state, PaneResourceState::Released);
        assert!(store.get("%33").unwrap().requires_seed);
    }

    #[test]
    fn a_new_snapshot_starts_a_fresh_raw_tail_epoch() {
        let mut store = PaneResourceStore::new(32, 1024);
        store.set_visible("%1", false, 1);
        store.snapshot("%1", b"first".to_vec(), 2);
        store.append("%1", b"old-tail", 3);
        store.snapshot("%1", b"second".to_vec(), 4);
        assert_eq!(store.get("%1").unwrap().serialized_snapshot, b"second");
        assert!(store.get("%1").unwrap().raw_tail.is_empty());
        store.append("%1", b"new-tail", 5);
        assert_eq!(store.get("%1").unwrap().raw_tail, b"new-tail");
    }

    #[test]
    fn repeated_hide_show_consumes_each_recovery_epoch_exactly_once() {
        let mut store = PaneResourceStore::new(32, 1024);
        store.set_visible("%1", false, 1);
        store.snapshot("%1", b"screen-one".to_vec(), 2);
        store.append("%1", b"tail-one", 3);
        store.set_visible("%1", true, 4);
        let first = store.take_recovery("%1").unwrap();
        assert_eq!(first.serialized_snapshot, b"screen-one");
        assert_eq!(first.raw_tail, b"tail-one");

        store.set_visible("%1", false, 5);
        store.snapshot("%1", b"screen-two".to_vec(), 6);
        store.append("%1", b"tail-two", 7);
        store.set_visible("%1", true, 8);
        let second = store.take_recovery("%1").unwrap();
        assert_eq!(second.serialized_snapshot, b"screen-two");
        assert_eq!(second.raw_tail, b"tail-two");
        assert_ne!(first.raw_tail, second.raw_tail);
        assert!(store.get("%1").unwrap().raw_tail.is_empty());
    }

    #[test]
    fn one_global_budget_bounds_one_hundred_never_visible_panes() {
        let mut store = PaneResourceStore::with_total_limit(32, 1024, 8 * 1024);
        for index in 0..100 {
            let pane_id = format!("%{index}");
            store.ensure(&pane_id, false, index);
            store.snapshot(&pane_id, vec![b's'; 512], index);
            store.append(&pane_id, &[b't'; 512], index);
        }
        assert!(store.retained_bytes() <= 8 * 1024);
        assert!(
            (0..100)
                .filter(|index| store.get(&format!("%{index}")).unwrap().requires_seed)
                .count()
                >= 68
        );
        assert!(store.is_hidden("%99"));
    }
}

#[cfg(test)]
#[path = "replay_phase14_tests.rs"]
mod phase14_incremental_tests;

#[cfg(test)]
#[path = "replay_phase14.rs"]
mod phase14_baseline_tests;
