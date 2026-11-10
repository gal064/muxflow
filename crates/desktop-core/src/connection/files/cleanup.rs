use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) enum CleanupStatus {
    NotNeeded,
    Removed,
    ConnectionClosed,
    Retained,
    Failed,
}

impl CleanupStatus {
    const fn severity(self) -> u8 {
        match self {
            Self::NotNeeded => 0,
            Self::Removed => 1,
            Self::ConnectionClosed => 2,
            Self::Retained => 3,
            Self::Failed => 4,
        }
    }
}

/// Canonical monotonic cleanup evidence. Once cleanup is uncertain or failed,
/// a later no-op cancel or missing partial can never claim a better outcome.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct CleanupReport {
    pub(super) status: CleanupStatus,
    pub(super) error: Option<String>,
}

impl CleanupReport {
    pub(super) fn new(status: CleanupStatus, error: Option<String>) -> Self {
        Self { status, error }
    }

    pub(super) fn merge(&mut self, status: CleanupStatus, error: Option<String>) {
        match status.severity().cmp(&self.status.severity()) {
            std::cmp::Ordering::Greater => {
                self.status = status;
                self.error = error;
            }
            std::cmp::Ordering::Equal if self.error.is_none() => self.error = error,
            std::cmp::Ordering::Equal | std::cmp::Ordering::Less => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cleanup_evidence_is_monotonic_and_preserves_first_equal_error() {
        let mut report = CleanupReport::new(CleanupStatus::Failed, Some("unlink denied".into()));
        report.merge(CleanupStatus::Removed, None);
        report.merge(CleanupStatus::NotNeeded, None);
        report.merge(CleanupStatus::Failed, Some("later failure".into()));
        assert_eq!(report.status, CleanupStatus::Failed);
        assert_eq!(report.error.as_deref(), Some("unlink denied"));

        let mut report = CleanupReport::new(CleanupStatus::ConnectionClosed, Some("closed".into()));
        report.merge(
            CleanupStatus::Retained,
            Some("owned backup retained".into()),
        );
        report.merge(CleanupStatus::Removed, None);
        assert_eq!(report.status, CleanupStatus::Retained);
        assert_eq!(report.error.as_deref(), Some("owned backup retained"));
    }
}
