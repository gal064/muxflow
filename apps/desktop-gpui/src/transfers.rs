//! The one transfer vocabulary: port of `features/transfers/transferState.ts`.
//!
//! Uploads and downloads use these words, so a backend outcome can never stay
//! styled as active because one surface did not know it. Serde rejects a value
//! the host may one day send that is not named here, as the TypeScript did.

use serde::{Deserialize, Deserializer, de::IntoDeserializer};

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum TransferState {
    Queued,
    Preflighting,
    Running,
    Verifying,
    Completed,
    Cancelled,
    Failed,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum TransferOutcome {
    Published,
    NotPublished,
    Unknown,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum TransferFailureKind {
    StaleScope,
    Transfer,
    Timeout,
    OutcomeUnknown,
    Cleanup,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum TransferCleanupStatus {
    NotNeeded,
    Removed,
    Retained,
    Failed,
    ConnectionClosed,
}

impl TransferCleanupStatus {
    /// How much the host has to say about leftovers: a worse report is never
    /// replaced by a better one.
    fn severity(self) -> u8 {
        match self {
            Self::NotNeeded => 0,
            Self::Removed => 1,
            Self::ConnectionClosed => 2,
            Self::Retained => 3,
            Self::Failed => 4,
        }
    }

    pub fn word(self) -> &'static str {
        match self {
            Self::NotNeeded => "notNeeded",
            Self::Removed => "removed",
            Self::Retained => "retained",
            Self::Failed => "failed",
            Self::ConnectionClosed => "connectionClosed",
        }
    }
}

/// A field the host leaves empty when it has nothing to say, which is not a
/// value to reject: `""` reads as absent, as `transferOutcomeFromWire` did.
pub fn empty_is_absent<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    match Option::<String>::deserialize(deserializer)? {
        None => Ok(None),
        Some(text) if text.is_empty() => Ok(None),
        Some(text) => T::deserialize(text.into_deserializer()).map(Some),
    }
}

impl TransferState {
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Cancelled | Self::Failed)
    }

    /// Once verification or commit begins, the backend owns the outcome.
    pub fn can_cancel(self) -> bool {
        matches!(self, Self::Queued | Self::Preflighting | Self::Running)
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Queued => "Queued",
            Self::Preflighting => "Checking destination",
            Self::Running => "Transferring",
            Self::Verifying => "Verifying and committing",
            Self::Completed => "Completed",
            Self::Cancelled => "Cancelled",
            Self::Failed => "Failed",
        }
    }

    /// Where an active state sits on its way to a terminal one.
    fn active_order(self) -> Option<u8> {
        match self {
            Self::Queued => Some(0),
            Self::Preflighting => Some(1),
            Self::Running => Some(2),
            Self::Verifying => Some(3),
            Self::Completed | Self::Cancelled | Self::Failed => None,
        }
    }
}

/// The lifecycle a late or repeated frame may not weaken: a finished transfer
/// stays finished, and an active one never steps back.
pub fn lifecycle_may_advance(current: TransferState, incoming: TransferState) -> bool {
    if current.is_terminal() {
        return false;
    }
    match (incoming.active_order(), current.active_order()) {
        (Some(incoming), Some(current)) => incoming >= current,
        _ => true,
    }
}

/// A cleanup report as it travels: the status and its message.
pub type Cleanup = (Option<TransferCleanupStatus>, Option<String>);

/// Merges two cleanup reports, keeping the worse status and, for equal ones,
/// the first message.
pub fn merge_cleanup(current: Cleanup, incoming: Cleanup) -> Cleanup {
    let (Some(incoming_status), incoming_error) = incoming.clone() else {
        return current;
    };
    let Some(current_status) = current.0 else {
        return (
            Some(incoming_status),
            incoming_error.filter(|e| !e.is_empty()),
        );
    };
    match incoming_status.severity().cmp(&current_status.severity()) {
        std::cmp::Ordering::Greater => (
            Some(incoming_status),
            incoming_error.filter(|e| !e.is_empty()),
        ),
        std::cmp::Ordering::Less => current,
        std::cmp::Ordering::Equal => (
            Some(current_status),
            current
                .1
                .filter(|e| !e.is_empty())
                .or(incoming_error.filter(|e| !e.is_empty())),
        ),
    }
}

/// Checks that a frame's state, outcome and failure kind agree. Phase 5's
/// download decode uses it; the upload list shows what the host says.
pub fn validate(
    state: TransferState,
    outcome: Option<TransferOutcome>,
    failure_kind: Option<TransferFailureKind>,
) -> Result<(), String> {
    if !state.is_terminal() {
        if outcome.is_some() || failure_kind.is_some() {
            return Err("An active transfer reported a terminal outcome.".into());
        }
        return Ok(());
    }
    let Some(outcome) = outcome else {
        return Err(format!(
            "Terminal transfer state {state:?} omitted its outcome."
        ));
    };
    if state == TransferState::Completed && outcome == TransferOutcome::Unknown {
        return Err("A completed transfer cannot have an unknown outcome.".into());
    }
    if state == TransferState::Cancelled && outcome != TransferOutcome::NotPublished {
        return Err("A cancelled transfer must be notPublished.".into());
    }
    if state == TransferState::Failed && failure_kind.is_none() {
        return Err("A failed transfer omitted its failure kind.".into());
    }
    if state != TransferState::Failed && failure_kind.is_some() {
        return Err("Only a failed transfer can carry a failure kind.".into());
    }
    let permits_unknown = matches!(
        failure_kind,
        Some(
            TransferFailureKind::OutcomeUnknown
                | TransferFailureKind::Timeout
                | TransferFailureKind::StaleScope
        )
    );
    if outcome == TransferOutcome::Unknown && !permits_unknown {
        return Err("An unknown publication outcome requires outcomeUnknown, timeout, or staleScope failure kind.".into());
    }
    if failure_kind == Some(TransferFailureKind::OutcomeUnknown)
        && outcome != TransferOutcome::Unknown
    {
        return Err("An outcomeUnknown failure must carry an unknown publication outcome.".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use TransferCleanupStatus as Cleanup;
    use TransferFailureKind as Kind;
    use TransferOutcome as Outcome;
    use TransferState as State;

    const STATES: [State; 7] = [
        State::Queued,
        State::Preflighting,
        State::Running,
        State::Verifying,
        State::Completed,
        State::Cancelled,
        State::Failed,
    ];

    #[test]
    fn maps_every_backend_state_and_rejects_future_ones() {
        let words = [
            "queued",
            "preflighting",
            "running",
            "verifying",
            "completed",
            "cancelled",
            "failed",
        ];
        for (word, state) in words.iter().zip(STATES) {
            assert_eq!(
                serde_json::from_value::<State>(serde_json::json!(word)).unwrap(),
                state
            );
        }
        for unknown in ["future-state", "progress"] {
            assert!(serde_json::from_value::<State>(serde_json::json!(unknown)).is_err());
        }
    }

    #[test]
    fn every_state_has_a_label_a_terminal_flag_and_a_cancellation_policy() {
        let labels: Vec<_> = STATES.iter().map(|state| state.label()).collect();
        assert_eq!(
            labels,
            [
                "Queued",
                "Checking destination",
                "Transferring",
                "Verifying and committing",
                "Completed",
                "Cancelled",
                "Failed"
            ]
        );
        let terminal: Vec<_> = STATES.into_iter().filter(|s| s.is_terminal()).collect();
        assert_eq!(
            terminal,
            [State::Completed, State::Cancelled, State::Failed]
        );
        let cancellable: Vec<_> = STATES.into_iter().filter(|s| s.can_cancel()).collect();
        assert_eq!(
            cancellable,
            [State::Queued, State::Preflighting, State::Running]
        );
    }

    #[test]
    fn an_empty_field_reads_as_absent_and_an_unknown_one_is_refused() {
        #[derive(Deserialize)]
        struct Frame {
            #[serde(default, deserialize_with = "empty_is_absent")]
            outcome: Option<Outcome>,
        }
        let parse = |text: &str| serde_json::from_str::<Frame>(text);
        assert_eq!(parse("{}").unwrap().outcome, None);
        assert_eq!(parse(r#"{"outcome": ""}"#).unwrap().outcome, None);
        assert_eq!(
            parse(r#"{"outcome": "notPublished"}"#).unwrap().outcome,
            Some(Outcome::NotPublished)
        );
        assert!(parse(r#"{"outcome": "maybe"}"#).is_err());
    }

    #[test]
    fn keeps_publication_outcome_distinct_from_lifecycle_state() {
        let ok = |state, outcome, kind| validate(state, outcome, kind).unwrap();
        let err = |state, outcome, kind| validate(state, outcome, kind).unwrap_err();
        ok(State::Running, None, None);
        ok(
            State::Failed,
            Some(Outcome::Unknown),
            Some(Kind::OutcomeUnknown),
        );
        ok(State::Failed, Some(Outcome::Unknown), Some(Kind::Timeout));
        ok(
            State::Failed,
            Some(Outcome::Unknown),
            Some(Kind::StaleScope),
        );
        ok(State::Cancelled, Some(Outcome::NotPublished), None);
        assert!(err(State::Completed, None, None).contains("omitted its outcome"));
        assert!(err(State::Failed, Some(Outcome::NotPublished), None).contains("failure kind"));
        assert!(
            err(State::Failed, Some(Outcome::Unknown), Some(Kind::Cleanup)).contains("requires")
        );
        assert!(
            err(
                State::Cancelled,
                Some(Outcome::NotPublished),
                Some(Kind::Transfer)
            )
            .contains("Only a failed transfer")
        );
        assert!(err(State::Cancelled, Some(Outcome::Published), None).contains("notPublished"));
        assert!(err(State::Running, Some(Outcome::Published), None).contains("active"));
        assert!(err(State::Completed, Some(Outcome::Unknown), None).contains("completed"));
        assert!(
            err(
                State::Failed,
                Some(Outcome::NotPublished),
                Some(Kind::OutcomeUnknown)
            )
            .contains("unknown publication outcome")
        );
    }

    #[test]
    fn a_late_frame_never_weakens_the_lifecycle() {
        // Terminal is final, even against another terminal state.
        assert!(!lifecycle_may_advance(State::Failed, State::Cancelled));
        assert!(!lifecycle_may_advance(State::Completed, State::Running));
        // Active states only move forward, and any terminal state ends them.
        assert!(lifecycle_may_advance(State::Running, State::Verifying));
        assert!(lifecycle_may_advance(State::Running, State::Running));
        assert!(!lifecycle_may_advance(State::Verifying, State::Running));
        assert!(lifecycle_may_advance(State::Verifying, State::Failed));
    }

    fn report(status: Cleanup, error: &str) -> super::Cleanup {
        (Some(status), (!error.is_empty()).then(|| error.to_owned()))
    }

    #[test]
    fn merges_cleanup_monotonically() {
        // The worse report wins, with its message.
        assert_eq!(
            merge_cleanup(
                report(Cleanup::Retained, "quarantined"),
                report(Cleanup::Failed, "rollback unavailable")
            ),
            report(Cleanup::Failed, "rollback unavailable")
        );
        // A better one, arriving late, changes nothing.
        let failed = report(Cleanup::Failed, "rollback failed");
        assert_eq!(
            merge_cleanup(failed.clone(), report(Cleanup::Removed, "")),
            failed
        );
        // Equal severity keeps the first message.
        assert_eq!(
            merge_cleanup(
                report(Cleanup::Failed, "first failure"),
                report(Cleanup::Failed, "later failure")
            ),
            report(Cleanup::Failed, "first failure")
        );
        assert_eq!(
            merge_cleanup(
                report(Cleanup::ConnectionClosed, "connection lost during cleanup"),
                report(Cleanup::Removed, "")
            ),
            report(Cleanup::ConnectionClosed, "connection lost during cleanup")
        );
        // Silence keeps what was known, and a first report is taken whole.
        assert_eq!(merge_cleanup(failed.clone(), (None, None)), failed);
        assert_eq!(
            merge_cleanup((None, None), report(Cleanup::Retained, "left")),
            report(Cleanup::Retained, "left")
        );
    }
}
