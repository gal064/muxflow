use serde::Serialize;
use serde_json::{Map, Value, json};

use super::cleanup::CleanupReport;
pub(super) use super::cleanup::CleanupStatus;
use super::scheduler::BulkBinding;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) enum TransferState {
    Queued,
    Preflighting,
    Running,
    Verifying,
    Completed,
    Cancelled,
    Failed,
}

impl TransferState {
    pub(super) fn terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Cancelled | Self::Failed)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) enum TransferOutcome {
    Published,
    NotPublished,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) enum TransferFailureKind {
    StaleScope,
    Transfer,
    Timeout,
    OutcomeUnknown,
    Cleanup,
}

/// The scheduler's canonical failure value. Publication outcome is deliberately
/// independent from the failure cause: a stale scope or timeout can occur either
/// before publication or while an authoritative commit result is unavailable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct TransferFailure {
    pub(super) outcome: TransferOutcome,
    pub(super) failure_kind: TransferFailureKind,
    pub(super) cleanup_status: CleanupStatus,
    pub(super) error: String,
    pub(super) cleanup_error: Option<String>,
}

pub(super) type TransferResult = Result<(), TransferFailure>;

impl TransferFailure {
    pub(super) fn new(
        outcome: TransferOutcome,
        failure_kind: TransferFailureKind,
        cleanup_status: CleanupStatus,
        error: impl Into<String>,
        cleanup_error: Option<String>,
    ) -> Self {
        Self {
            outcome,
            failure_kind,
            cleanup_status,
            error: error.into(),
            cleanup_error,
        }
    }

    pub(super) fn not_published(error: impl Into<String>) -> Self {
        Self::new(
            TransferOutcome::NotPublished,
            TransferFailureKind::Transfer,
            CleanupStatus::NotNeeded,
            error,
            None,
        )
    }

    pub(super) fn unknown(failure_kind: TransferFailureKind, error: impl Into<String>) -> Self {
        let error = error.into();
        Self::new(
            TransferOutcome::Unknown,
            failure_kind,
            CleanupStatus::Retained,
            error.clone(),
            Some(error),
        )
    }

    pub(super) fn merge_cleanup(&mut self, status: CleanupStatus, error: Option<String>) {
        let mut report = CleanupReport::new(self.cleanup_status, self.cleanup_error.take());
        report.merge(status, error);
        self.cleanup_status = report.status;
        self.cleanup_error = report.error;
    }
}

impl From<String> for TransferFailure {
    fn from(error: String) -> Self {
        Self::not_published(error)
    }
}

impl From<&str> for TransferFailure {
    fn from(error: &str) -> Self {
        Self::not_published(error)
    }
}

pub(super) struct TransferEvent {
    value: Map<String, Value>,
}

impl TransferEvent {
    pub(super) fn new(id: &str, binding: &BulkBinding, state: TransferState) -> Self {
        let Value::Object(value) = json!({
            "transferId": id,
            "state": state,
            "serverIdentity": binding.expected_server_identity,
            "expectedServerIdentity": binding.expected_server_identity,
            "connectionEpoch": binding.connection_epoch.to_string(),
            "terminal": state.terminal(),
        }) else {
            unreachable!()
        };
        Self { value }
    }

    pub(super) fn outcome(mut self, outcome: TransferOutcome) -> Self {
        self.value.insert("outcome".into(), json!(outcome));
        self
    }

    pub(super) fn failure(mut self, kind: TransferFailureKind, error: String) -> Self {
        self.value.insert("failureKind".into(), json!(kind));
        self.value.insert("error".into(), Value::String(error));
        self
    }

    pub(super) fn error(mut self, error: impl Into<String>) -> Self {
        self.value
            .insert("error".into(), Value::String(error.into()));
        self
    }

    pub(super) fn cleanup(mut self, status: CleanupStatus, error: Option<String>) -> Self {
        self.value.insert("cleanupStatus".into(), json!(status));
        if let Some(error) = error {
            self.value
                .insert("cleanupError".into(), Value::String(error));
        }
        self
    }

    pub(super) fn fields(mut self, extra: Value) -> Self {
        if let Value::Object(extra) = extra {
            for (key, value) in extra {
                if !matches!(
                    key.as_str(),
                    "transferId"
                        | "state"
                        | "serverIdentity"
                        | "expectedServerIdentity"
                        | "connectionEpoch"
                        | "terminal"
                ) {
                    self.value.insert(key, value);
                }
            }
        }
        self
    }

    pub(super) fn value(self) -> Value {
        Value::Object(self.value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_state_spellings_match_the_frontend_contract() {
        assert_eq!(
            serde_json::to_string(&TransferState::Queued).unwrap(),
            "\"queued\""
        );
        assert_eq!(
            serde_json::to_string(&TransferState::Preflighting).unwrap(),
            "\"preflighting\""
        );
        assert_eq!(
            serde_json::to_string(&TransferState::Running).unwrap(),
            "\"running\""
        );
        assert_eq!(
            serde_json::to_string(&TransferState::Verifying).unwrap(),
            "\"verifying\""
        );
        assert_eq!(
            serde_json::to_string(&TransferState::Completed).unwrap(),
            "\"completed\""
        );
        assert_eq!(
            serde_json::to_string(&TransferState::Cancelled).unwrap(),
            "\"cancelled\""
        );
        assert_eq!(
            serde_json::to_string(&TransferState::Failed).unwrap(),
            "\"failed\""
        );
        assert_eq!(
            serde_json::to_string(&TransferOutcome::Published).unwrap(),
            "\"published\""
        );
        assert_eq!(
            serde_json::to_string(&TransferOutcome::NotPublished).unwrap(),
            "\"notPublished\""
        );
        assert_eq!(
            serde_json::to_string(&TransferOutcome::Unknown).unwrap(),
            "\"unknown\""
        );
        assert_eq!(
            serde_json::to_string(&TransferFailureKind::StaleScope).unwrap(),
            "\"staleScope\""
        );
        assert_eq!(
            serde_json::to_string(&TransferFailureKind::OutcomeUnknown).unwrap(),
            "\"outcomeUnknown\""
        );
        assert_eq!(
            serde_json::to_string(&CleanupStatus::ConnectionClosed).unwrap(),
            "\"connectionClosed\""
        );
    }

    #[test]
    fn cancelled_terminal_event_has_not_published_outcome_and_no_failure_kind() {
        use std::sync::{Arc, atomic::Ordering};

        let client = Arc::new(crate::connection::TerminalClient::new());
        client.ready.store(true, Ordering::Release);
        client.terminal_epoch.store(91, Ordering::Release);
        *client.server_identity.lock().unwrap() = "test-server".into();
        let binding = BulkBinding {
            client,
            expected_server_identity: "test-server".into(),
            connection_epoch: 91,
        };
        let event = TransferEvent::new("cancelled-transfer", &binding, TransferState::Cancelled)
            .outcome(TransferOutcome::NotPublished)
            .cleanup(CleanupStatus::Removed, None)
            .error("cancelled by user")
            .value();
        assert_eq!(event["state"], "cancelled");
        assert_eq!(event["outcome"], "notPublished");
        assert_eq!(event["cleanupStatus"], "removed");
        assert!(event.get("failureKind").is_none());
        assert_eq!(event["terminal"], true);
    }
}
