use std::{
    collections::HashSet,
    sync::{Arc, Mutex, atomic::AtomicBool, mpsc},
};

use tmux_control::PaneResourceStore;

use super::{CommandBlock, PaneSeedState, StreamControl, StreamState, with_active_resources};

impl StreamState {
    pub(in crate::service::terminal) fn apply_control(
        &mut self,
        control: StreamControl,
        capture_in_flight: &Mutex<HashSet<String>>,
    ) {
        match control {
            StreamControl::Membership { pane_ids } => {
                let desired: HashSet<_> = pane_ids.iter().map(String::as_str).collect();
                let removed: Vec<_> = self
                    .pane_states
                    .keys()
                    .filter(|pane_id| !desired.contains(pane_id.as_str()))
                    .cloned()
                    .collect();
                for pane_id in removed {
                    self.pane_states.remove(&pane_id);
                    self.release_correlation(Some(&pane_id));
                    // A pane this client no longer owns is not one it can
                    // resume, and leaving it here would make the *next* pane to
                    // take its id inherit a pause that was never its own.
                    self.flow.cleared(&pane_id);
                    // Its capture, likewise. The drain below fences only the
                    // pane whose capture is the block that happens to be open;
                    // a pane removed with no open block of its own is never
                    // fenced, so without this a pane id that is later re-added
                    // would inherit an entry nothing clears, and every seed it
                    // asked for would be coalesced against a photograph nobody
                    // is taking.
                    capture_in_flight.lock().unwrap().remove(&pane_id);
                    // The parser still considers an in-flight tmux command
                    // open until its matching `%end` or `%error`. Drain that
                    // fence without retaining or publishing stale capture
                    // rows. Clearing only the stream side made the remaining
                    // rows look like unframed output; preserving the capture
                    // itself let a remove/re-add publish an obsolete seed.
                    if self.active_scope() == pane_id
                        && let Some(tag) = self.active_tag()
                    {
                        self.command_block = CommandBlock::Draining { tag };
                    }
                }
                for pane_id in pane_ids {
                    self.pane_states
                        .entry(pane_id)
                        .or_insert_with(|| PaneSeedState::Pending {
                            buffered: Vec::new(),
                            buffered_bytes: 0,
                            overflowed: false,
                        });
                }
            }
        }
    }

    pub(super) fn resnapshot_all(
        &mut self,
        writer: &mpsc::Sender<super::super::ControlWrite>,
        resources: &Arc<Mutex<PaneResourceStore>>,
        stopped: &AtomicBool,
    ) {
        self.release_correlation(None);
        if let Some(tag) = self.active_tag() {
            // A parser error can occur inside an open command block. The
            // parser retains that tag until the real fence, so the stream
            // state must do the same while discarding the damaged payload.
            self.command_block = CommandBlock::Draining { tag };
        }
        // A pane the renderer is not showing is not photographed here. Its
        // screen would be discarded on the way out — only a visible pane's seed
        // is emitted — and its reveal takes a fresh photograph anyway, so the
        // capture is work tmux does for nobody. What the pane does get is the
        // seed debt, because this recovery began with bytes the stream lost:
        // its reveal must not be answered with a tail that is missing the
        // middle of itself.
        //
        // A pane tmux has paused is captured regardless of who is showing it.
        // The resume rides on the capture, and losing one leaves tmux holding
        // that pane's output forever.
        let unphotographed: HashSet<String> = with_active_resources(resources, stopped, |store| {
            let hidden: HashSet<String> = self
                .pane_states
                .keys()
                .filter(|pane_id| {
                    // A pane the store has never heard of is not "hidden", it
                    // is unaccounted for, and the safe answer to that is the
                    // photograph.
                    store.get(pane_id).is_some()
                        && store.is_hidden(pane_id)
                        && !self.flow.resume_before_capture(pane_id)
                })
                .cloned()
                .collect();
            for pane_id in &hidden {
                store.require_seed(
                    pane_id,
                    "the control stream was resnapshotted while this pane was hidden",
                );
            }
            hidden
        })
        .unwrap_or_default();
        for (pane_id, state) in &mut self.pane_states {
            if unphotographed.contains(pane_id) {
                // Nothing is coming to seed this pane, so it must not sit
                // Pending accumulating output nobody will replay. Live is what
                // a hidden pane's steady state already is: the store takes what
                // arrives, and a released resource drops it.
                *state = PaneSeedState::Live;
                continue;
            }
            *state = PaneSeedState::Pending {
                buffered: Vec::new(),
                buffered_bytes: 0,
                overflowed: false,
            };
            // A parse failure is the one recovery that re-captures every pane at
            // once, and a paused one among them still needs its resume: the
            // whole point of this pass is that afterwards every pane is
            // delivering again.
            super::request_capture(writer, pane_id, self.flow.resume_before_capture(pane_id));
        }
    }
}
