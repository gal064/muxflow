use std::{collections::HashSet, sync::mpsc};

use super::{CommandBlock, PaneSeedState, StreamControl, StreamState};

impl StreamState {
    pub(in crate::service::terminal) fn apply_control(&mut self, control: StreamControl) {
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
                    if self.expected_capture.as_deref() == Some(&pane_id) {
                        self.expected_capture = None;
                    }
                    if self.expected_resume.as_deref() == Some(&pane_id) {
                        self.expected_resume = None;
                    }
                    // A pane this client no longer owns is not one it can
                    // resume, and leaving it here would make the *next* pane to
                    // take its id inherit a pause that was never its own.
                    self.flow.cleared(&pane_id);
                    if self
                        .pending_alternate
                        .as_ref()
                        .is_some_and(|pending| pending.0 == pane_id)
                    {
                        self.pending_alternate = None;
                    }
                    if self
                        .pending_metadata
                        .as_ref()
                        .is_some_and(|pending| pending.pane_id == pane_id)
                    {
                        self.pending_metadata = None;
                    }
                    if self.active_scope() == pane_id {
                        self.command_block = CommandBlock::None;
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

    pub(super) fn resnapshot_all(&mut self, writer: &mpsc::Sender<super::super::ControlWrite>) {
        self.expected_capture = None;
        self.expected_resume = None;
        self.pending_alternate = None;
        self.pending_metadata = None;
        self.command_block = CommandBlock::None;
        for (pane_id, state) in &mut self.pane_states {
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
