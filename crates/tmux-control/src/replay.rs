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
    pub raw_tail: Vec<u8>,
    pub generation: u64,
    pub snapshot_generation: u64,
    pub tail_through_generation: u64,
    pub requires_seed: bool,
    /// The renderer's own cached screen is the recovery base, and `raw_tail` is
    /// the complete output since the checkpoint this store recorded for it.
    /// Never set together with `requires_seed`.
    pub resume_from_renderer: bool,
    pub recovery_reason: String,
}

/// The largest tail a reveal answers with instead of a photograph.
///
/// A screen-only capture of a 200x50 pane is ~10 KB, and that is what the
/// alternative answer costs. Below about one and a half screens the tail is
/// both cheaper and better — it preserves the renderer's scrollback continuity
/// rather than replacing its buffer — and above it the screen is cheaper and
/// fresher. So the per-switch worst case is `panes-in-window * 16 KiB` (~48 KB
/// for a three-pane window) and the ordinary case, an idle hidden pane, is
/// zero. A pane that outgrows this is not a fault: it is a busy pane, and the
/// answer to a busy pane is a photograph.
pub const REVEAL_TAIL_BOUND: usize = 16 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputDisposition {
    Visible,
    Hidden,
    Released,
}

/// Why a pane lost its host-side recovery material without anyone asking.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaneDegradationCause {
    /// The global retained-byte budget or the hidden-pane LRU evicted it.
    GlobalBudget,
    /// A hidden pane's raw output tail outgrew the per-pane budget.
    HiddenTailOverflow,
}

/// One pane whose recovery material this store discarded on its own.
///
/// The store cannot emit protocol events — it is a pure data structure below
/// the service — but a discard it performs silently is exactly how a pane ends
/// up waiting forever for bytes nobody will send. Every such discard is
/// recorded here for the owner of the store to drain and report, which is what
/// [`PaneResourceStore::take_degradations`] exists for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaneDegradation {
    pub pane_id: String,
    pub cause: PaneDegradationCause,
    /// The pane's state *after* the discard.
    pub state: PaneResourceState,
    pub generation: u64,
    pub reason: String,
}

/// Degradations retained while nobody drains them.
///
/// One entry per pane (the latest wins, because "this pane needs a seed" is
/// idempotent), so this bound is only ever reached by a store holding more
/// panes than any topology this host attaches to.
const MAX_RECORDED_PANE_DEGRADATIONS: usize = 256;

/// Why a reveal is answered with a photograph rather than a tail.
const UNVERIFIED_REVEAL_REASON: &str =
    "the reveal did not match the renderer handoff this host recorded";

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
    /// The newest sequence [`PaneResourceStore::trim_journal_to_tail_bound`]
    /// dropped from a pane's journal, for as long as that journal exists.
    journal_trimmed_through: HashMap<String, u64>,
    handoff_checkpoints: HashMap<String, VisibilityCheckpoint>,
    degradations: Vec<PaneDegradation>,
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
            journal_trimmed_through: HashMap::new(),
            handoff_checkpoints: HashMap::new(),
            degradations: Vec::new(),
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
                raw_tail: Vec::new(),
                generation,
                snapshot_generation: generation,
                tail_through_generation: generation,
                requires_seed: false,
                resume_from_renderer: false,
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
            self.forget_journal(pane_id);
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

    /// Atomically transfers renderer ownership to the host, storing no copy of
    /// the renderer's screen.
    ///
    /// The renderer keeps that screen; what this records is the checkpoint it
    /// kept it at, and the output observed after that cutoff — retained exactly
    /// once, and only up to [`REVEAL_TAIL_BOUND`]. The checkpoint is the whole
    /// authority for the reveal: without a stored screen, the only thing that
    /// makes a tail safe to write is agreement about which screen it continues.
    pub fn hide_with_checkpoint(
        &mut self,
        pane_id: &str,
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
        // Trimming drops the *oldest* entries, and this cutoff is normally
        // newer than all of them: the renderer was drawing that output as it
        // arrived, so a tail starting above the watermark quotes nothing that
        // was dropped and has lost nothing. Only a cutoff below the last
        // sequence dropped leaves a hole in the tail, and only that costs this
        // pane its resume.
        let trimmed_past_cutoff = self
            .journal_trimmed_through
            .remove(pane_id)
            .is_some_and(|watermark| checkpoint.generation < watermark);
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
        let exceeds_resource = tail.len() > self.max_resource_bytes;
        let resource = self.resources.get_mut(pane_id).expect("resource ensured");
        resource.generation = tail_through_generation.max(generation);
        resource.snapshot_generation = checkpoint.generation;
        resource.tail_through_generation = tail_through_generation;
        if resource.requires_seed || trimmed_past_cutoff {
            release(resource, "visible output handoff journal was not retained");
        } else if exceeds_resource {
            release(resource, "renderer handoff exceeded the hidden-pane budget");
        } else {
            resource.state = PaneResourceState::HiddenBuffered;
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

    /// Makes a pane renderer-owned and answers its reveal exactly once.
    ///
    /// `holding` is the checkpoint the renderer says it is still showing, or
    /// `None` from a renderer that kept nothing. The tail is handed back only
    /// when that checkpoint is the one this store recorded for the handoff and
    /// the pane is still buffering against it; every other case — a renderer
    /// holding nothing, a new epoch, an eviction, a [`Self::require_seed`], a
    /// reveal for a handoff this host never saw, a pane already visible — is
    /// answered with `requires_seed` and no bytes at all. Since the host keeps
    /// no copy of the screen, that agreement is the only thing that makes a
    /// tail safe to write, and a tail written onto the wrong screen is a defect
    /// nothing later repairs.
    pub fn reveal(
        &mut self,
        pane_id: &str,
        generation: u64,
        holding: Option<VisibilityCheckpoint>,
    ) -> Option<PaneResource> {
        let before = self.accounted_state(pane_id);
        let resumes = holding.is_some()
            && self.handoff_checkpoints.get(pane_id).copied() == holding
            && self.resources.get(pane_id).is_some_and(|resource| {
                resource.state == PaneResourceState::HiddenBuffered && !resource.requires_seed
            });
        let resource = self.resources.get_mut(pane_id)?;
        // A pane this store already believes is renderer-owned has no handoff
        // to verify and no bytes to hand back: this is a re-assertion of a
        // visibility nothing took away — the degraded-pane watchdog's, or a
        // reveal the desktop sent twice. Answering it like a fresh reveal would
        // stamp `requires_seed` onto a working pane, which costs a capture and
        // blanks the buffer it is drawing correctly. Its own seed debt, if it
        // has one, is reported unchanged.
        if resource.state == PaneResourceState::Visible {
            resource.generation = generation;
            return Some(PaneResource {
                state: PaneResourceState::Visible,
                raw_tail: Vec::new(),
                generation,
                snapshot_generation: generation,
                tail_through_generation: generation,
                requires_seed: resource.requires_seed,
                resume_from_renderer: false,
                recovery_reason: resource.recovery_reason.clone(),
            });
        }
        let answer = if resumes {
            PaneResource {
                state: PaneResourceState::Visible,
                raw_tail: std::mem::take(&mut resource.raw_tail),
                generation,
                snapshot_generation: resource.snapshot_generation,
                tail_through_generation: resource.tail_through_generation,
                requires_seed: false,
                resume_from_renderer: true,
                recovery_reason: String::new(),
            }
        } else {
            resource.raw_tail.clear();
            PaneResource {
                state: PaneResourceState::Visible,
                raw_tail: Vec::new(),
                generation,
                snapshot_generation: generation,
                tail_through_generation: generation,
                requires_seed: true,
                resume_from_renderer: false,
                recovery_reason: if resource.recovery_reason.is_empty() {
                    UNVERIFIED_REVEAL_REASON.to_owned()
                } else {
                    resource.recovery_reason.clone()
                },
            }
        };
        resource.state = PaneResourceState::Visible;
        resource.generation = generation;
        resource.snapshot_generation = generation;
        resource.tail_through_generation = generation;
        resource.requires_seed = !resumes;
        resource.recovery_reason = answer.recovery_reason.clone();
        self.forget_journal(pane_id);
        self.handoff_checkpoints.remove(pane_id);
        self.refresh_accounting(pane_id, before);
        Some(answer)
    }

    /// Makes a pane renderer-owned because the renderer explicitly asked for a
    /// seed of it.
    ///
    /// A seed request *is* the statement of visibility: nothing asks for a
    /// screen it is not about to draw. Without this, a pane whose resource was
    /// hidden or released — by a renderer handoff the desktop has since
    /// forgotten, or by an eviction it was never told about — has its completed
    /// capture stored and never emitted, and the request produces nothing at
    /// all, forever.
    ///
    /// The hidden recovery material is discarded exactly as [`Self::reveal`]
    /// discards it: the authoritative seed that follows replaces it, and
    /// keeping a stale snapshot beside a fresher one is what would then be
    /// replayed twice. Returns whether the pane actually had to be forced,
    /// which is the only part worth counting.
    pub fn reveal_for_seed_request(&mut self, pane_id: &str, generation: u64) -> bool {
        match self.resources.get(pane_id).map(|resource| resource.state) {
            Some(PaneResourceState::Visible) => {
                self.reveal(pane_id, generation, None);
                false
            }
            Some(_) => {
                self.reveal(pane_id, generation, None);
                true
            }
            // A pane with no resource at all is hidden by `is_hidden`'s own
            // definition, so its seed would be suppressed just the same.
            None => {
                self.ensure(pane_id, true, generation);
                true
            }
        }
    }

    /// Records that an authoritative seed was captured for this pane, without
    /// keeping a copy of it.
    ///
    /// The seed goes to the renderer, which is the only side that needs a
    /// screen; what the host keeps is the boundary it is current through, so
    /// the tail that follows starts a fresh epoch and the pane stops owing a
    /// seed. Storing the bytes as well would put a whole screen back inside the
    /// per-pane bound this store now enforces — the seed would be refused for
    /// being too large, which releases the pane, which asks for another seed.
    ///
    /// The recorded handoff goes with it, and that is the whole of why this is
    /// safe for a pane that is *hidden*. A capture can complete over a hidden
    /// pane — a resnapshot, a flow-control pause — and the seed it produces is
    /// discarded on the way out, because only a visible pane's seed is emitted.
    /// Leaving the checkpoint behind would let the reveal that follows match it
    /// and answer "resume, nothing printed while you were away" with the empty
    /// tail this call just cleared: everything the pane printed while hidden,
    /// silently gone. Without the checkpoint the reveal cannot match, so it
    /// answers with a photograph.
    pub fn seeded(&mut self, pane_id: &str, generation: u64) {
        self.ensure(pane_id, false, generation);
        let before = self.accounted_state(pane_id);
        self.forget_journal(pane_id);
        self.handoff_checkpoints.remove(pane_id);
        let resource = self.resources.get_mut(pane_id).expect("resource ensured");
        resource.raw_tail.clear();
        resource.snapshot_generation = generation;
        resource.tail_through_generation = generation;
        resource.requires_seed = false;
        resource.recovery_reason.clear();
        resource.generation = generation;
        self.refresh_accounting(pane_id, before);
        self.enforce_limits();
    }

    /// Forgets a visible pane's handoff journal and everything derived from it.
    ///
    /// The trim watermark is part of that journal: it says which of *these*
    /// entries were dropped, and it means nothing about the entries a later
    /// visible window records.
    fn forget_journal(&mut self, pane_id: &str) {
        self.output_journals.remove(pane_id);
        self.output_journal_bytes.remove(pane_id);
        self.journal_trimmed_through.remove(pane_id);
    }

    /// Drops the oldest journal entries a hide could never hand back anyway.
    ///
    /// A visible pane's journal is read for one purpose: the tail a hide gives
    /// the host, which is the output after the renderer's cutoff and is bounded
    /// by `max_resource_bytes`. Anything older than the last bound's worth of
    /// bytes can only ever be part of a tail that overflows that bound, and an
    /// overflowing tail is answered with a photograph rather than with a
    /// truncated one. Keeping it is how a build log in one visible pane grew
    /// until the global budget evicted some *other* pane's recovery material —
    /// a seed and a blank frame for a pane that was working.
    ///
    /// What is dropped is recorded as a watermark rather than as a verdict.
    /// These are the *oldest* entries, and the cutoff a later hide carries is
    /// normally newer than all of them — an active pane's renderer is drawing
    /// the output as it arrives — so a tail that quotes nothing below the
    /// watermark has lost nothing. Only [`Self::hide_with_checkpoint`] knows
    /// which cutoff it was, so only it can say whether this cost the pane its
    /// resume; deciding here instead is what put a blank frame and a seed in
    /// front of most switches back to a busy pane.
    fn trim_journal_to_tail_bound(&mut self, pane_id: &str) {
        let Some(bytes) = self.output_journal_bytes.get_mut(pane_id) else {
            return;
        };
        if *bytes <= self.max_resource_bytes {
            return;
        }
        let Some(journal) = self.output_journals.get_mut(pane_id) else {
            return;
        };
        let mut trimmed_through = None;
        while *bytes > self.max_resource_bytes {
            let Some(oldest) = journal.pop_front() else {
                break;
            };
            *bytes -= oldest.bytes.len();
            trimmed_through = Some(oldest.sequence);
        }
        if let Some(sequence) = trimmed_through {
            let watermark = self
                .journal_trimmed_through
                .entry(pane_id.to_owned())
                .or_default();
            *watermark = (*watermark).max(sequence);
        }
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
                self.trim_journal_to_tail_bound(pane_id);
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
        const OVERFLOW_REASON: &str = "raw output tail exceeded the hidden-pane budget";
        let released =
            if resource.raw_tail.len().saturating_add(bytes.len()) > self.max_resource_bytes {
                release(resource, OVERFLOW_REASON);
                true
            } else {
                resource.raw_tail.extend_from_slice(bytes);
                resource.tail_through_generation = generation;
                false
            };
        resource.generation = generation;
        if released {
            // Recorded, and — unlike an eviction — not reported to the desktop.
            // A hidden pane outgrowing `REVEAL_TAIL_BOUND` is the expected
            // outcome for a busy pane, not a fault: its reveal answers with a
            // photograph, which is fresher than the tail would have been.
            // Emitting here would put an ordered pane-resource event in front
            // of every switch for every noisy background pane, which is the
            // traffic this bound exists to remove. The counter still moves.
            self.record_degradation(PaneDegradation {
                pane_id: pane_id.to_owned(),
                cause: PaneDegradationCause::HiddenTailOverflow,
                state: PaneResourceState::Released,
                generation,
                reason: OVERFLOW_REASON.into(),
            });
        }
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
            raw_tail: std::mem::take(&mut resource.raw_tail),
            generation: resource.generation,
            snapshot_generation: resource.snapshot_generation,
            tail_through_generation: resource.tail_through_generation,
            requires_seed: resource.requires_seed,
            resume_from_renderer: false,
            recovery_reason: resource.recovery_reason.clone(),
        };
        self.refresh_accounting(pane_id, before);
        Some(recovery)
    }

    pub fn remove(&mut self, pane_id: &str) {
        let before = self.accounted_state(pane_id);
        self.forget_journal(pane_id);
        self.handoff_checkpoints.remove(pane_id);
        self.resources.remove(pane_id);
        self.refresh_accounting(pane_id, before);
    }

    /// Invalidates a visibility handoff whose ordered event could not be
    /// admitted. Output must not observe the speculative Visible/Hidden state
    /// after its recovery boundary was lost; the next mount repairs from an
    /// authoritative seed instead.
    pub fn require_seed(&mut self, pane_id: &str, reason: &str) {
        self.ensure(pane_id, false, 0);
        let before = self.accounted_state(pane_id);
        self.forget_journal(pane_id);
        self.handoff_checkpoints.remove(pane_id);
        release(
            self.resources.get_mut(pane_id).expect("resource ensured"),
            reason,
        );
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
        let resource_bytes = resource.map_or(0, |resource| resource.raw_tail.len());
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
            self.forget_journal(&pane_id);
            let degradation = self.resources.get_mut(&pane_id).map(|resource| {
                record_pane_resource_measurement!(|measurements: &mut PaneResourceMeasurements| {
                    measurements.evictions += 1;
                });
                let reason = if over_bytes {
                    "global pane-resource byte budget was exceeded"
                } else {
                    "global pane-resource LRU capacity was exceeded"
                };
                if resource.state == PaneResourceState::Visible {
                    resource.raw_tail.clear();
                    resource.requires_seed = true;
                    resource.recovery_reason = reason.into();
                } else {
                    release(resource, reason);
                }
                PaneDegradation {
                    pane_id: pane_id.clone(),
                    cause: PaneDegradationCause::GlobalBudget,
                    state: resource.state,
                    generation: resource.generation,
                    reason: reason.into(),
                }
            });
            // An eviction is a decision this store makes about a pane nobody
            // asked it about. Recorded rather than emitted, because emitting
            // here would mean holding this store's lock across the event path
            // the emitter takes.
            if let Some(degradation) = degradation {
                self.record_degradation(degradation);
            }
            self.refresh_accounting(&pane_id, before);
        }
    }

    /// Retains one pane's latest degradation, replacing any earlier one.
    ///
    /// Latest-wins per pane because the signal is idempotent — "this pane needs
    /// an authoritative seed" does not become truer by being recorded twice —
    /// and because that is what bounds this list without a drain.
    fn record_degradation(&mut self, degradation: PaneDegradation) {
        if let Some(existing) = self
            .degradations
            .iter_mut()
            .find(|existing| existing.pane_id == degradation.pane_id)
        {
            *existing = degradation;
            return;
        }
        if self.degradations.len() >= MAX_RECORDED_PANE_DEGRADATIONS {
            self.degradations.remove(0);
        }
        self.degradations.push(degradation);
    }

    /// Takes every degradation recorded since the last drain, for the caller to
    /// report once it no longer holds this store.
    pub fn take_degradations(&mut self) -> Vec<PaneDegradation> {
        std::mem::take(&mut self.degradations)
    }
}

fn release(resource: &mut PaneResource, reason: &str) {
    resource.state = PaneResourceState::Released;
    resource.raw_tail.clear();
    resource.requires_seed = true;
    resource.resume_from_renderer = false;
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
        let hidden = store.hide_with_checkpoint("%1", checkpoint, 12).unwrap();
        assert_eq!(hidden.raw_tail, b"between");
        assert_eq!(hidden.snapshot_generation, 10);
        assert_eq!(hidden.tail_through_generation, 11);
        assert_eq!(
            store.record_output("%1", b"after", 13),
            OutputDisposition::Hidden
        );
        let repeated = store.hide_with_checkpoint("%1", checkpoint, 14).unwrap();
        assert_eq!(repeated.raw_tail, b"betweenafter");
        assert_eq!(repeated.snapshot_generation, 10);
        assert_eq!(repeated.tail_through_generation, 13);
        let recovery = store.reveal("%1", 15, Some(checkpoint)).unwrap();
        assert!(recovery.resume_from_renderer);
        assert_eq!(recovery.raw_tail, b"betweenafter");
        assert_eq!(recovery.snapshot_generation, 10);
        assert_eq!(recovery.tail_through_generation, 13);
        // The same reveal arriving twice — a retry, a remount, the watchdog
        // re-asserting — finds a pane this store already calls renderer-owned.
        // The tail is handed back exactly once, so the second answer carries no
        // bytes; it carries no new debt either, because forcing a seed onto a
        // pane that is drawing correctly costs a capture and an `ESC c`. A
        // renderer that genuinely has nothing to show asks for its own seed —
        // it is the only side that knows whether it is holding a screen.
        let repeated_reveal = store.reveal("%1", 16, Some(checkpoint)).unwrap();
        assert!(repeated_reveal.raw_tail.is_empty());
        assert!(!repeated_reveal.requires_seed);
    }

    /// The rule the switch-payload work turns around: the host stops holding a
    /// copy of the renderer's screen, so a hide that carries no bytes is the
    /// *ordinary* hide rather than a broken one. What the host keeps is the
    /// output the renderer had not yet seen, and the renderer's own cache is
    /// the base that output is written on top of.
    ///
    #[test]
    fn a_hide_without_a_snapshot_buffers_the_tail_instead_of_releasing() {
        let mut store = PaneResourceStore::with_total_limit(32, 1024, 4096);
        store.ensure("%1", true, 1);
        assert_eq!(
            store.record_output("%1", b"before", 10),
            OutputDisposition::Visible
        );
        assert_eq!(
            store.record_output("%1", b"after", 11),
            OutputDisposition::Visible
        );
        let hidden = store
            .hide_with_checkpoint(
                "%1",
                VisibilityCheckpoint {
                    epoch: 7,
                    generation: 10,
                },
                12,
            )
            .unwrap();
        assert_eq!(hidden.state, PaneResourceState::HiddenBuffered);
        assert!(!hidden.requires_seed);
        // Exactly the output the renderer had not drawn when it let go, and
        // nothing it had.
        assert_eq!(hidden.raw_tail, b"after");
        assert_eq!(hidden.snapshot_generation, 10);
    }

    /// The tail is the whole answer a reveal carries, so it has to be exact in
    /// both directions: every byte after the checkpoint, and each of them once.
    ///
    #[test]
    fn a_reveal_answers_the_exact_output_after_the_checkpoint_exactly_once() {
        let mut store = PaneResourceStore::with_total_limit(32, 1024, 4096);
        store.ensure("%1", true, 1);
        for (generation, bytes) in [
            (30, b"one".as_slice()),
            (31, b"two".as_slice()),
            (32, b"three".as_slice()),
        ] {
            assert_eq!(
                store.record_output("%1", bytes, generation),
                OutputDisposition::Visible
            );
        }
        let checkpoint = VisibilityCheckpoint {
            epoch: 4,
            generation: 29,
        };
        store.hide_with_checkpoint("%1", checkpoint, 33).unwrap();
        let revealed = store.reveal("%1", 34, Some(checkpoint)).unwrap();
        assert!(revealed.resume_from_renderer);
        assert_eq!(revealed.raw_tail, b"onetwothree");
        assert_eq!(revealed.snapshot_generation, checkpoint.generation);
        assert_eq!(revealed.tail_through_generation, 32);
        // A second reveal is the same reveal arriving twice — a retry, a
        // remount — and replaying the tail again would double every byte.
        let repeated = store.reveal("%1", 35, Some(checkpoint)).unwrap();
        assert!(repeated.raw_tail.is_empty());
    }

    /// Past the bound the tail is no longer the cheaper answer, and a truncated
    /// one is worse than none: it splices bytes onto a screen with a hole in the
    /// middle that nothing later repairs. The pane is released instead and the
    /// reveal asks for a fresh photograph.
    ///
    /// The store's per-resource bound is `REVEAL_TAIL_BOUND` in production; a
    /// small one here keeps the test about the boundary rather than about
    /// allocating.
    #[test]
    fn a_tail_past_the_reveal_bound_requires_a_seed_and_never_a_partial_tail() {
        let mut store = PaneResourceStore::with_total_limit(32, 64, 4096);
        store.ensure("%1", true, 1);
        let checkpoint = VisibilityCheckpoint {
            epoch: 2,
            generation: 1,
        };
        store.hide_with_checkpoint("%1", checkpoint, 2).unwrap();
        store.append("%1", &[b'x'; 32], 3);
        store.append("%1", &[b'y'; 64], 4);
        let revealed = store.reveal("%1", 5, Some(checkpoint)).unwrap();
        assert!(revealed.requires_seed);
        assert!(!revealed.resume_from_renderer);
        assert!(
            revealed.raw_tail.is_empty(),
            "a tail past the bound must be dropped whole, never truncated"
        );
    }

    /// A busy *visible* pane is the ordinary case, and it must not cost some
    /// other pane its recovery material.
    ///
    /// Its journal is only ever read to compute the tail a hide hands over, and
    /// that tail is bounded — so everything past the bound is dropped as it
    /// arrives rather than retained until the global budget evicts a stranger.
    /// The pane that printed is the only one that pays, and it pays with a
    /// photograph instead of a tail.
    #[test]
    fn a_visible_panes_journal_stays_within_the_tail_bound_and_the_hide_says_so() {
        let mut store = PaneResourceStore::with_total_limit(32, 64, 4096);
        store.ensure("%1", true, 1);
        for generation in 10..20 {
            assert_eq!(
                store.record_output("%1", &[b'x'; 16], generation),
                OutputDisposition::Visible
            );
        }
        assert!(store.journal_bytes() <= 64);
        assert!(
            store.take_degradations().is_empty(),
            "a pane printing to the screen is not a degradation"
        );
        let hidden = store
            .hide_with_checkpoint(
                "%1",
                VisibilityCheckpoint {
                    epoch: 1,
                    generation: 10,
                },
                21,
            )
            .unwrap();
        assert_eq!(hidden.state, PaneResourceState::Released);
        assert!(hidden.requires_seed);
        assert!(hidden.raw_tail.is_empty());
    }

    /// The other side of the trim comparison, and the ordinary one.
    ///
    /// Trimming drops the oldest entries, and an active pane's renderer has
    /// already drawn them: the cutoff a hide carries is newer than all of them,
    /// so the tail quotes nothing that was dropped. Condemning the pane on the
    /// fact of a trim alone put a blank frame and a seed in front of most
    /// switches back to a busy pane.
    #[test]
    fn a_hide_whose_cutoff_is_newer_than_every_trimmed_byte_still_answers_with_a_tail() {
        let mut store = PaneResourceStore::with_total_limit(32, 64, 4096);
        store.ensure("%1", true, 1);
        for generation in 10..20 {
            assert_eq!(
                store.record_output("%1", &[b'x'; 16], generation),
                OutputDisposition::Visible
            );
        }
        assert!(store.journal_bytes() <= 64);
        let hidden = store
            .hide_with_checkpoint(
                "%1",
                VisibilityCheckpoint {
                    epoch: 1,
                    generation: 16,
                },
                21,
            )
            .unwrap();
        assert_eq!(hidden.state, PaneResourceState::HiddenBuffered);
        assert!(!hidden.requires_seed);
        assert_eq!(hidden.raw_tail, [b'x'; 48]);
        assert_eq!(hidden.tail_through_generation, 19);
    }

    /// Re-asserting a visibility nothing took away costs the pane nothing.
    ///
    /// The degraded-pane watchdog re-sends a reveal for a pane it believes the
    /// host is holding, and a desktop can send one twice. Treating that as a
    /// fresh reveal stamps seed debt onto a pane that is drawing correctly: a
    /// capture the user did not need, and an `ESC c` that blanks the buffer on
    /// the way to redrawing what was already there.
    #[test]
    fn a_reveal_of_a_pane_the_host_already_calls_visible_changes_nothing() {
        let mut store = PaneResourceStore::with_total_limit(32, 1024, 4096);
        store.ensure("%1", true, 1);
        assert_eq!(
            store.record_output("%1", b"on-screen", 2),
            OutputDisposition::Visible
        );

        let answer = store.reveal("%1", 3, None).expect("resource");
        assert!(!answer.requires_seed);
        assert!(!answer.resume_from_renderer);

        // And the handoff journal is still there, so the next hide answers with
        // a tail rather than with a photograph.
        let hidden = store
            .hide_with_checkpoint(
                "%1",
                VisibilityCheckpoint {
                    epoch: 1,
                    generation: 1,
                },
                4,
            )
            .unwrap();
        assert_eq!(hidden.state, PaneResourceState::HiddenBuffered);
        assert_eq!(hidden.raw_tail, b"on-screen");
    }

    /// A seed the renderer never received must not be mistaken for one it did.
    ///
    /// A capture can complete over a *hidden* pane — a resnapshot after a parse
    /// error, a flow-control pause — and the screen it produces is discarded,
    /// because only a visible pane's seed is emitted. What it also discards is
    /// the tail. If the recorded handoff survived that, the reveal would match
    /// it and answer "resume: nothing printed while you were away" with an
    /// empty tail, and everything the pane printed while hidden would be gone
    /// with no sign that anything was lost.
    #[test]
    fn a_seed_completed_over_a_hidden_pane_leaves_its_reveal_asking_for_one() {
        let mut store = PaneResourceStore::with_total_limit(32, 1024, 4096);
        store.ensure("%1", true, 1);
        let checkpoint = VisibilityCheckpoint {
            epoch: 1,
            generation: 1,
        };
        store.hide_with_checkpoint("%1", checkpoint, 2).unwrap();
        assert_eq!(
            store.record_output("%1", b"printed-while-hidden", 3),
            OutputDisposition::Hidden
        );

        store.seeded("%1", 4);

        let answer = store.reveal("%1", 5, Some(checkpoint)).expect("resource");
        assert!(
            answer.requires_seed,
            "a reveal whose tail was discarded must ask for a photograph"
        );
        assert!(!answer.resume_from_renderer);
        assert!(answer.raw_tail.is_empty());
    }

    /// The invariant that replaces the uploaded snapshot as the authority.
    ///
    /// Once the host holds no copy of the screen, the only thing that makes a
    /// tail safe to apply is agreement about *which* screen it applies to. The
    /// recorded handoff checkpoint is that agreement, and anything else — an
    /// epoch change, an eviction, a `require_seed`, a reveal for a handoff this
    /// host never saw — is answered with a seed rather than with bytes.
    ///
    #[test]
    fn a_reveal_whose_checkpoint_the_host_did_not_record_requires_a_seed() {
        let mut store = PaneResourceStore::with_total_limit(32, 1024, 4096);
        store.ensure("%1", true, 1);
        assert_eq!(
            store.record_output("%1", b"tail", 10),
            OutputDisposition::Visible
        );
        let recorded = VisibilityCheckpoint {
            epoch: 7,
            generation: 9,
        };
        store.hide_with_checkpoint("%1", recorded, 11).unwrap();
        assert_eq!(store.handoff_checkpoints.get("%1"), Some(&recorded));
        // The renderer comes back on a new epoch, so the screen it is holding
        // is not the one this tail continues.
        let revealed = store
            .reveal(
                "%1",
                13,
                Some(VisibilityCheckpoint {
                    epoch: 8,
                    generation: 9,
                }),
            )
            .unwrap();
        assert!(revealed.requires_seed);
        assert!(!revealed.resume_from_renderer);
        assert!(revealed.raw_tail.is_empty());

        // And the renderer that kept no screen at all — an oversized
        // serialization its cache declined, a mount with nothing cached — is
        // the same answer for the same reason.
        store.ensure("%2", true, 1);
        assert_eq!(
            store.record_output("%2", b"tail", 14),
            OutputDisposition::Visible
        );
        store
            .hide_with_checkpoint(
                "%2",
                VisibilityCheckpoint {
                    epoch: 7,
                    generation: 13,
                },
                15,
            )
            .unwrap();
        let revealed = store.reveal("%2", 16, None).unwrap();
        assert!(revealed.requires_seed);
        assert!(revealed.raw_tail.is_empty());
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
        let checkpoint = VisibilityCheckpoint {
            epoch: 3,
            generation: 20,
        };
        let recovery = store.hide_with_checkpoint("%1", checkpoint, 22).unwrap();
        assert_eq!(
            store.record_output("%1", b"D", 23),
            OutputDisposition::Hidden
        );
        let recovery = store.reveal("%1", 24, Some(checkpoint)).unwrap_or(recovery);
        // The screen this tail continues is the renderer's, not the host's.
        let screen_the_renderer_kept = b"screen+A";
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
        let combined = [
            screen_the_renderer_kept.to_vec(),
            recovery.raw_tail,
            remaining,
        ]
        .concat();
        assert_eq!(combined, b"screen+ABCDE");
    }

    #[test]
    fn renderer_handoff_rejects_a_cutoff_not_observed_for_that_pane() {
        let mut store = PaneResourceStore::new(32, 1024);
        store.ensure("%1", true, 5);
        let error = store
            .hide_with_checkpoint(
                "%1",
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
            store.record_output(&pane, &[b'x'; 1024], index);
        }
        assert!(store.retained_bytes() <= 8 * 1024);
        assert!(store.resources.values().any(|resource| {
            resource.state == PaneResourceState::Visible && resource.requires_seed
        }));
    }

    #[test]
    fn evicting_a_visible_pane_surfaces_a_degradation_the_caller_can_report() {
        let mut store = PaneResourceStore::with_total_limit(32, 1024, 2 * 1024);
        for index in 0..4_u64 {
            let pane = format!("%{index}");
            store.ensure(&pane, true, index);
            store.record_output(&pane, &[b'x'; 1024], index);
        }
        let degradations = store.take_degradations();
        assert!(
            !degradations.is_empty(),
            "a visible pane lost its recovery material silently"
        );
        for degradation in &degradations {
            assert_eq!(degradation.cause, PaneDegradationCause::GlobalBudget);
            assert!(degradation.reason.contains("budget") || degradation.reason.contains("LRU"));
            assert!(
                store
                    .get(&degradation.pane_id)
                    .is_some_and(|resource| resource.requires_seed)
            );
        }
        // Draining is what bounds the list; a second drain reports nothing new.
        assert!(store.take_degradations().is_empty());
    }

    /// Recorded by the store, and — since the tail bound became the ordinary
    /// limit of a busy hidden pane rather than a fault — reported to the
    /// desktop by nobody. The record is what keeps the counter honest and what
    /// makes the release visible to a test; the event it used to become is what
    /// put a pane-resource frame in front of every switch.
    #[test]
    fn hidden_tail_overflow_surfaces_its_release() {
        let mut store = PaneResourceStore::with_total_limit(32, 16, 4096);
        store.set_visible("%1", false, 1);
        store.seeded("%1", 2);
        assert!(store.take_degradations().is_empty());
        store.append("%1", &[b'x'; 64], 3);
        let degradations = store.take_degradations();
        assert_eq!(degradations.len(), 1);
        assert_eq!(degradations[0].pane_id, "%1");
        assert_eq!(
            degradations[0].cause,
            PaneDegradationCause::HiddenTailOverflow
        );
        assert_eq!(degradations[0].state, PaneResourceState::Released);
        assert_eq!(degradations[0].generation, 3);
        assert_eq!(store.get("%1").unwrap().state, PaneResourceState::Released);
    }

    #[test]
    fn an_explicit_seed_request_reveals_a_released_pane_so_its_snapshot_can_be_emitted() {
        let mut store = PaneResourceStore::with_total_limit(32, 1024, 4096);
        store.set_visible("%1", false, 1);
        store.seeded("%1", 2);
        store.require_seed("%1", "test");
        assert_eq!(store.get("%1").unwrap().state, PaneResourceState::Released);
        assert!(store.is_hidden("%1"));

        assert!(store.reveal_for_seed_request("%1", 3));
        assert_eq!(store.get("%1").unwrap().state, PaneResourceState::Visible);
        assert!(!store.is_hidden("%1"));
        // The seed capture that follows lands on a pane the emission gate now
        // passes, and clears the debt it was requested for.
        store.seeded("%1", 4);
        assert!(!store.is_hidden("%1"));
        assert!(!store.get("%1").unwrap().requires_seed);

        // Already visible: nothing forced, and nothing counted.
        assert!(!store.reveal_for_seed_request("%1", 5));
        // Never mounted here at all: still forced, because an absent resource
        // is hidden by `is_hidden`'s definition.
        assert!(store.reveal_for_seed_request("%9", 6));
        assert!(!store.is_hidden("%9"));
    }

    #[test]
    fn a_hidden_pane_revealed_for_a_seed_request_discards_its_stale_recovery_material() {
        let mut store = PaneResourceStore::with_total_limit(32, 1024, 4096);
        store.ensure("%1", true, 1);
        store
            .hide_with_checkpoint(
                "%1",
                VisibilityCheckpoint {
                    epoch: 1,
                    generation: 1,
                },
                1,
            )
            .unwrap();
        store.append("%1", b"tail", 2);
        assert!(store.reveal_for_seed_request("%1", 3));
        let resource = store.get("%1").unwrap();
        assert_eq!(resource.state, PaneResourceState::Visible);
        assert!(resource.raw_tail.is_empty());
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

    /// The per-pane bound is a tail bound now: there is no stored screen left
    /// for it to measure, and a pane that outgrows it is released for the next
    /// reveal to photograph.
    #[test]
    fn an_oversized_tail_and_a_thirty_third_hidden_pane_require_seed() {
        let mut store = PaneResourceStore::new(32, 4 * 1024 * 1024);
        store.set_visible("%0", false, 1);
        store.append("%0", &vec![b'x'; 4 * 1024 * 1024 + 1], 2);
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
    fn a_new_seed_starts_a_fresh_raw_tail_epoch() {
        let mut store = PaneResourceStore::new(32, 1024);
        store.set_visible("%1", false, 1);
        store.seeded("%1", 2);
        store.append("%1", b"old-tail", 3);
        store.seeded("%1", 4);
        assert_eq!(store.get("%1").unwrap().snapshot_generation, 4);
        assert!(store.get("%1").unwrap().raw_tail.is_empty());
        store.append("%1", b"new-tail", 5);
        assert_eq!(store.get("%1").unwrap().raw_tail, b"new-tail");
    }

    #[test]
    fn repeated_hide_show_consumes_each_recovery_epoch_exactly_once() {
        let mut store = PaneResourceStore::new(32, 1024);
        store.set_visible("%1", false, 1);
        store.seeded("%1", 2);
        store.append("%1", b"tail-one", 3);
        store.set_visible("%1", true, 4);
        let first = store.take_recovery("%1").unwrap();
        assert_eq!(first.raw_tail, b"tail-one");

        store.set_visible("%1", false, 5);
        store.seeded("%1", 6);
        store.append("%1", b"tail-two", 7);
        store.set_visible("%1", true, 8);
        let second = store.take_recovery("%1").unwrap();
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
            store.append(&pane_id, &[b't'; 1024], index);
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
