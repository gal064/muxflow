use std::fmt;

/// Why a filesystem request failed, as a value rather than a message prefix.
///
/// The wire error code used to be recovered by matching the first word of a
/// human-facing string produced modules away, so renaming a message silently
/// reclassified a cancellation as a rejection. The code is now part of the
/// error's identity and the message is only its rendering.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FileFailure {
    /// The caller withdrew the request while it was running.
    Cancelled,
    /// A directory page token belongs to another listing, or to none.
    StalePageToken,
    /// The file moved out from under the exact version the caller asked for.
    StaleGeneration,
    /// Everything else the service refused.
    Rejected,
}

impl FileFailure {
    pub(crate) fn code(self) -> &'static str {
        match self {
            Self::Cancelled => "cancelled",
            Self::StalePageToken => "stale_page_token",
            Self::StaleGeneration => "stale_file_generation",
            Self::Rejected => "",
        }
    }

    /// Classifies an error, or `Rejected` when nothing in the chain claims one.
    pub(crate) fn of(error: &anyhow::Error) -> Self {
        error
            .chain()
            .find_map(|cause| cause.downcast_ref::<FileError>())
            .map_or(Self::Rejected, |typed| typed.failure)
    }
}

#[derive(Debug)]
pub(crate) struct FileError {
    failure: FileFailure,
    message: String,
}

impl fmt::Display for FileError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for FileError {}

fn refuse(failure: FileFailure, message: impl Into<String>) -> anyhow::Error {
    anyhow::Error::new(FileError {
        failure,
        message: message.into(),
    })
}

pub(super) fn cancelled(what: &str) -> anyhow::Error {
    refuse(FileFailure::Cancelled, format!("{what} was cancelled"))
}

pub(super) fn stale_page_token(detail: &str) -> anyhow::Error {
    refuse(FileFailure::StalePageToken, detail)
}

pub(super) fn stale_generation(detail: &str) -> anyhow::Error {
    refuse(FileFailure::StaleGeneration, detail)
}
