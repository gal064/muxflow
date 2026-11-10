//! The pane reveal reducer. Port of `PaneRevealState.ts` and `PaneRecovery.ts`.

use crate::event::{PaneEvent, PaneResource, PaneResourceState};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PaneRevealState {
    pub ready: bool,
    pub has_local_state: bool,
}

#[derive(Debug, PartialEq, Eq)]
pub enum PaneRecoveryPlan {
    None,
    AwaitSeed {
        reason: String,
    },
    Resume {
        raw_tail: Vec<u8>,
        snapshot_generation: u64,
        tail_through_generation: u64,
    },
}

pub fn pane_recovery_plan(event: &mut PaneResource) -> PaneRecoveryPlan {
    if event.requires_seed || event.state == PaneResourceState::Released {
        return PaneRecoveryPlan::AwaitSeed {
            reason: std::mem::take(&mut event.recovery_reason),
        };
    }
    // The flag, never the byte count: a verified reveal of a pane that printed
    // nothing while hidden is zero bytes and still the whole recovery.
    if event.resume_from_renderer {
        return PaneRecoveryPlan::Resume {
            raw_tail: std::mem::take(&mut event.raw_tail),
            snapshot_generation: event.snapshot_generation,
            tail_through_generation: event.tail_through_generation,
        };
    }
    PaneRecoveryPlan::None
}

/// Output the tail has not already covered.
pub fn output_after_recovery<T>(
    output: Vec<T>,
    tail_through_generation: u64,
    generation: impl Fn(&T) -> u64,
) -> Vec<T> {
    output
        .into_iter()
        .filter(|item| generation(item) > tail_through_generation)
        .collect()
}

#[derive(Debug, PartialEq, Eq)]
pub enum PaneRevealEffect {
    None,
    Seed(Vec<u8>),
    Output(Vec<u8>),
    DeferOutput(Vec<u8>),
    /// Draw what you are already holding, then these bytes. An empty tail is
    /// still the ordered barrier that acknowledges the reveal.
    Resume {
        raw_tail: Vec<u8>,
        snapshot_generation: u64,
        tail_through_generation: u64,
    },
    AwaitSeed {
        reason: String,
        request_seed: bool,
        /// Set only when the answer was in a state this reducer has no rule for.
        unusable_state: Option<PaneResourceState>,
    },
    /// This pane's own hide, echoed into a mount still waiting for its reveal.
    AwaitAnswer {
        showing_screen: bool,
    },
    /// The same echo at a pane already drawing. Acting on it wiped a correct
    /// screen, so it is ignored and recorded.
    HideEchoAfterReady,
    Diagnostic(String),
}

fn unusable_reason(state: PaneResourceState) -> String {
    format!(
        "The host answered this pane's reveal with no usable renderer state ({})",
        state.name()
    )
}

pub fn reduce_pane_reveal(
    state: PaneRevealState,
    event: PaneEvent,
) -> (PaneRevealState, PaneRevealEffect) {
    const NOTHING_HELD: PaneRevealState = PaneRevealState {
        ready: false,
        has_local_state: false,
    };
    let mut resource = match event {
        PaneEvent::SeedDiagnostic { message, .. } => {
            return (state, PaneRevealEffect::Diagnostic(message));
        }
        PaneEvent::Seed { data, .. } => {
            return (
                PaneRevealState {
                    ready: true,
                    has_local_state: true,
                },
                PaneRevealEffect::Seed(data),
            );
        }
        PaneEvent::Output { data, .. } => {
            let effect = if state.ready {
                PaneRevealEffect::Output(data)
            } else {
                PaneRevealEffect::DeferOutput(data)
            };
            return (state, effect);
        }
        PaneEvent::History { .. } => return (state, PaneRevealEffect::None),
        PaneEvent::Resource(resource) => resource,
    };

    match pane_recovery_plan(&mut resource) {
        PaneRecoveryPlan::Resume {
            raw_tail,
            snapshot_generation,
            tail_through_generation,
        } => {
            if !state.has_local_state {
                return (
                    NOTHING_HELD,
                    PaneRevealEffect::AwaitSeed {
                        reason: "The host resumed this pane from a screen it is no longer holding"
                            .into(),
                        request_seed: true,
                        unusable_state: None,
                    },
                );
            }
            return (
                PaneRevealState {
                    ready: true,
                    has_local_state: true,
                },
                PaneRevealEffect::Resume {
                    raw_tail,
                    snapshot_generation,
                    tail_through_generation,
                },
            );
        }
        PaneRecoveryPlan::AwaitSeed { reason } => {
            return (
                NOTHING_HELD,
                PaneRevealEffect::AwaitSeed {
                    reason,
                    request_seed: !resource.requires_seed,
                    unusable_state: None,
                },
            );
        }
        PaneRecoveryPlan::None => {}
    }
    if resource.state == PaneResourceState::Visible {
        if state.has_local_state {
            return (
                PaneRevealState {
                    ready: true,
                    ..state
                },
                PaneRevealEffect::None,
            );
        }
        return (
            NOTHING_HELD,
            PaneRevealEffect::AwaitSeed {
                reason: "No recoverable renderer state was available".into(),
                request_seed: true,
                unusable_state: None,
            },
        );
    }
    // `hiddenBuffered` is only ever this pane's own hide coming back: every
    // path a reveal can return by stamps `Visible`.
    if resource.state == PaneResourceState::HiddenBuffered {
        if state.ready {
            return (state, PaneRevealEffect::HideEchoAfterReady);
        }
        return (
            state,
            PaneRevealEffect::AwaitAnswer {
                showing_screen: state.has_local_state,
            },
        );
    }
    // An answer this build has no rule for is seed debt. Returning nothing here
    // left the pane deferring output for the rest of its life.
    (
        NOTHING_HELD,
        PaneRevealEffect::AwaitSeed {
            reason: unusable_reason(resource.state),
            request_seed: true,
            unusable_state: Some(resource.state),
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hidden_resource() -> PaneResource {
        PaneResource {
            pane_id: "%1".into(),
            state: PaneResourceState::HiddenBuffered,
            requires_seed: false,
            resume_from_renderer: false,
            recovery_reason: String::new(),
            generation: 1,
            snapshot_generation: 0,
            tail_through_generation: 1,
            raw_tail: vec![2],
            sequence: 1,
        }
    }

    fn resume_answer() -> PaneResource {
        PaneResource {
            state: PaneResourceState::Visible,
            generation: 21,
            snapshot_generation: 20,
            tail_through_generation: 20,
            raw_tail: Vec::new(),
            resume_from_renderer: true,
            ..hidden_resource()
        }
    }

    fn hide_echo() -> PaneResource {
        PaneResource {
            raw_tail: Vec::new(),
            ..hidden_resource()
        }
    }

    fn output(generation: u64, data: Vec<u8>) -> PaneEvent {
        PaneEvent::Output {
            pane_id: "%1".into(),
            generation,
            data,
            sequence: 1,
        }
    }

    const NOT_READY_HOLDING: PaneRevealState = PaneRevealState {
        ready: false,
        has_local_state: true,
    };
    const NOT_READY_EMPTY: PaneRevealState = PaneRevealState {
        ready: false,
        has_local_state: false,
    };
    const READY: PaneRevealState = PaneRevealState {
        ready: true,
        has_local_state: true,
    };

    #[test]
    fn defers_output_until_the_visibility_response_supplies_recovery() {
        let (state, effect) = reduce_pane_reveal(NOT_READY_HOLDING, output(1, vec![1]));
        assert!(matches!(effect, PaneRevealEffect::DeferOutput(_)));
        let (state, effect) = reduce_pane_reveal(state, PaneEvent::Resource(resume_answer()));
        assert!(matches!(effect, PaneRevealEffect::Resume { .. }));
        assert!(state.ready);
    }

    #[test]
    fn drops_output_already_represented_by_the_raw_tail_and_keeps_only_later_generations() {
        let tail = vec![66, 67, 68];
        let (_, effect) = reduce_pane_reveal(
            NOT_READY_HOLDING,
            PaneEvent::Resource(PaneResource {
                state: PaneResourceState::Visible,
                resume_from_renderer: true,
                generation: 24,
                snapshot_generation: 20,
                tail_through_generation: 23,
                raw_tail: tail.clone(),
                ..hidden_resource()
            }),
        );
        assert_eq!(
            effect,
            PaneRevealEffect::Resume {
                raw_tail: tail.clone(),
                snapshot_generation: 20,
                tail_through_generation: 23,
            }
        );
        let deferred = vec![
            (21, vec![66]),
            (22, vec![67]),
            (23, vec![68]),
            (25, vec![69]),
        ];
        let mut combined = tail;
        for (_, data) in output_after_recovery(deferred, 23, |item: &(u64, Vec<u8>)| item.0) {
            combined.extend(data);
        }
        assert_eq!(combined, b"BCDE");
    }

    #[test]
    fn waits_for_the_host_triggered_reseed_after_a_released_resource() {
        let (state, effect) = reduce_pane_reveal(
            NOT_READY_HOLDING,
            PaneEvent::Resource(PaneResource {
                state: PaneResourceState::Released,
                requires_seed: true,
                recovery_reason: "host LRU eviction".into(),
                raw_tail: Vec::new(),
                ..hidden_resource()
            }),
        );
        assert_eq!(state, NOT_READY_EMPTY);
        assert_eq!(
            effect,
            PaneRevealEffect::AwaitSeed {
                reason: "host LRU eviction".into(),
                request_seed: false,
                unusable_state: None,
            }
        );
    }

    #[test]
    fn makes_the_pane_ready_on_a_zero_byte_resume_answer() {
        let (state, effect) =
            reduce_pane_reveal(NOT_READY_HOLDING, PaneEvent::Resource(resume_answer()));
        assert_eq!(state, READY);
        assert_eq!(
            effect,
            PaneRevealEffect::Resume {
                raw_tail: Vec::new(),
                snapshot_generation: 20,
                tail_through_generation: 20,
            }
        );
    }

    #[test]
    fn asks_for_a_seed_when_a_resume_answer_reaches_a_pane_holding_no_screen() {
        let (state, effect) =
            reduce_pane_reveal(NOT_READY_EMPTY, PaneEvent::Resource(resume_answer()));
        assert_eq!(state, NOT_READY_EMPTY);
        assert!(matches!(
            effect,
            PaneRevealEffect::AwaitSeed {
                request_seed: true,
                ..
            }
        ));
    }

    #[test]
    fn ignores_the_hide_echo_replayed_into_a_pane_that_is_showing_its_cached_screen() {
        let (state, effect) =
            reduce_pane_reveal(NOT_READY_HOLDING, PaneEvent::Resource(hide_echo()));
        assert_eq!(
            effect,
            PaneRevealEffect::AwaitAnswer {
                showing_screen: true
            }
        );
        assert_eq!(state, NOT_READY_HOLDING);
    }

    #[test]
    fn bounds_the_wait_instead_of_blanking_a_pane_that_has_no_screen_to_show() {
        let (state, effect) = reduce_pane_reveal(NOT_READY_EMPTY, PaneEvent::Resource(hide_echo()));
        assert_eq!(
            effect,
            PaneRevealEffect::AwaitAnswer {
                showing_screen: false
            }
        );
        assert_eq!(state, NOT_READY_EMPTY);
    }

    #[test]
    fn ignores_the_hide_echo_at_a_pane_that_is_already_drawing_and_says_so() {
        let (state, effect) = reduce_pane_reveal(READY, PaneEvent::Resource(hide_echo()));
        assert_eq!(effect, PaneRevealEffect::HideEchoAfterReady);
        assert_eq!(state, READY);
    }

    #[test]
    fn treats_a_seed_diagnostic_as_nonfatal_and_leaves_reveal_readiness_unchanged() {
        let (state, effect) = reduce_pane_reveal(
            READY,
            PaneEvent::SeedDiagnostic {
                pane_id: "%1".into(),
                message: "partial metadata".into(),
                sequence: 1,
            },
        );
        assert_eq!(state, READY);
        assert_eq!(
            effect,
            PaneRevealEffect::Diagnostic("partial metadata".into())
        );
    }

    #[test]
    fn treats_an_unspecified_answer_as_seed_debt_rather_than_a_dead_end() {
        let (state, effect) = reduce_pane_reveal(
            NOT_READY_HOLDING,
            PaneEvent::Resource(PaneResource {
                state: PaneResourceState::Unspecified,
                raw_tail: Vec::new(),
                ..hidden_resource()
            }),
        );
        assert_eq!(state, NOT_READY_EMPTY);
        assert_eq!(
            effect,
            PaneRevealEffect::AwaitSeed {
                reason: "The host answered this pane's reveal with no usable renderer state (unspecified)".into(),
                request_seed: true,
                unusable_state: Some(PaneResourceState::Unspecified),
            }
        );
    }
}
