//! Downloads from the host: their decode, the per-connection list, the status
//! they announce, and the Explorer's Downloads section. Port of
//! `downloadFlow.ts`, `downloadStatus.ts`, `useConnectionTransfers.ts`,
//! `DownloadTransfers.tsx`, `DownloadActions.tsx` and `mapDownloadEvent`.

use serde::Deserialize;
use serde_json::Value;

use crate::{
    notices,
    transfers::{
        TransferCleanupStatus, TransferFailureKind, TransferOutcome, TransferState,
        lifecycle_may_advance, merge_cleanup, validate,
    },
};

use super::wire::{FileScope, decimal};

/// How many transfers one connection keeps in view.
const MAX_LISTED: usize = 100;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DownloadKind {
    File,
    Folder,
}

/// What the user pointed at on the host. Where it lands is the flow's job.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DownloadIntent {
    pub path: String,
    pub kind: DownloadKind,
}

/// One download, as the list shows it. Port of `TransferStatus`.
#[derive(Clone, Debug, PartialEq)]
pub struct DownloadStatus {
    pub id: String,
    /// The connection it belongs to ([`FileScope::transfer_key`]).
    pub scope_key: String,
    pub path: String,
    pub destination: Option<String>,
    pub kind: DownloadKind,
    pub state: TransferState,
    pub outcome: Option<TransferOutcome>,
    pub failure_kind: Option<TransferFailureKind>,
    pub completed_bytes: u64,
    pub total_bytes: Option<u64>,
    pub bytes_per_second: Option<u64>,
    pub eta_seconds: Option<f64>,
    pub error: Option<String>,
    pub cleanup_error: Option<String>,
    pub cleanup_status: Option<TransferCleanupStatus>,
}

impl DownloadStatus {
    /// What a download that could not even start, or whose event could not be
    /// read, reports: failed, with nothing published.
    pub fn refused(
        id: String,
        scope_key: String,
        intent: &DownloadIntent,
        destination: Option<String>,
        error: String,
    ) -> Self {
        Self {
            id,
            scope_key,
            path: intent.path.clone(),
            destination,
            kind: intent.kind,
            state: TransferState::Failed,
            outcome: Some(TransferOutcome::NotPublished),
            failure_kind: Some(TransferFailureKind::Transfer),
            completed_bytes: 0,
            total_bytes: None,
            bytes_per_second: None,
            eta_seconds: None,
            error: Some(error),
            cleanup_error: None,
            cleanup_status: None,
        }
    }

    /// A repeated or late frame never weakens the lifecycle, and cleanup keeps
    /// the worse report.
    fn merged(self, incoming: Self) -> Self {
        let cleanup = merge_cleanup(
            (self.cleanup_status, self.cleanup_error.clone()),
            (incoming.cleanup_status, incoming.cleanup_error.clone()),
        );
        let mut merged = if lifecycle_may_advance(self.state, incoming.state) {
            incoming
        } else {
            self
        };
        (merged.cleanup_status, merged.cleanup_error) = cleanup;
        merged
    }

    /// `"3.0 MiB / 10.0 MiB · 1.0 MiB/s · 7s remaining"`.
    pub fn progress_text(&self) -> String {
        use crate::terminal::integration::transfers::format_bytes;
        let bytes = match self.total_bytes {
            Some(total) => format!(
                "{} / {}",
                format_bytes(self.completed_bytes),
                format_bytes(total)
            ),
            None => format!("{} transferred", format_bytes(self.completed_bytes)),
        };
        let speed = self
            .bytes_per_second
            .map(|speed| format!(" · {}/s", format_bytes(speed)))
            .unwrap_or_default();
        let eta = self
            .eta_seconds
            .filter(|eta| *eta > 0.)
            .map(|eta| format!(" · {}s remaining", eta.ceil() as u64))
            .unwrap_or_default();
        format!("{bytes}{speed}{eta}")
    }

    /// Tenths of a percent, when the total is known.
    pub fn progress_permille(&self) -> Option<u32> {
        let total = self.total_bytes?;
        Some(if total == 0 {
            0
        } else {
            (u128::from(self.completed_bytes.min(total)) * 1000 / u128::from(total)) as u32
        })
    }
}

#[derive(Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct WireDownloadEvent {
    transfer_id: String,
    state: String,
    transferred_bytes: Option<Value>,
    total_bytes: Option<Value>,
    total_known: Option<bool>,
    throughput_bytes_per_second: Option<Value>,
    eta_seconds: Option<f64>,
    destination: Option<String>,
    error: Option<String>,
    cleanup_error: Option<String>,
    cleanup_status: Option<String>,
    outcome: Option<String>,
    failure_kind: Option<String>,
    server_identity: Option<String>,
    expected_server_identity: Option<String>,
    connection_epoch: Option<String>,
    terminal: Option<bool>,
}

fn word<T: for<'de> Deserialize<'de>>(
    value: &Option<String>,
    what: &str,
) -> Result<Option<T>, String> {
    match value.as_deref() {
        None | Some("") => Ok(None),
        Some(text) => serde_json::from_value(Value::String(text.to_owned()))
            .map(Some)
            .map_err(|_| format!("Unknown transfer {what}: {text}")),
    }
}

fn checked(value: &Value, label: &str) -> Result<u64, String> {
    let text = match value {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    };
    decimal(&text).ok_or_else(|| format!("Host returned an invalid {label}."))
}

/// One download event, refused when it belongs to another connection or does
/// not hang together. Port of `mapDownloadEvent`.
fn map_event(
    event: &WireDownloadEvent,
    intent: &DownloadIntent,
    destination: &str,
    scope: &FileScope,
) -> Result<DownloadStatus, String> {
    let identity = Some(scope.server_identity.as_str());
    if event.server_identity.as_deref() != identity
        || event.expected_server_identity.as_deref() != identity
        || event.connection_epoch.as_deref() != Some(&scope.terminal_epoch.to_string())
    {
        return Err("Host returned a download event for a stale connection scope.".into());
    }
    let state: TransferState = serde_json::from_value(Value::String(event.state.clone()))
        .map_err(|_| format!("Unknown transfer state: {}", event.state))?;
    if event.terminal != Some(state.is_terminal()) {
        return Err("Host returned an inconsistent download terminal marker.".into());
    }
    let outcome: Option<TransferOutcome> = word(&event.outcome, "outcome")?;
    if outcome.is_none() && state.is_terminal() {
        return Err(format!(
            "Terminal transfer state {} omitted its outcome.",
            event.state
        ));
    }
    let failure_kind = word(&event.failure_kind, "failure kind")?;
    let cleanup_status = word(&event.cleanup_status, "cleanup status")?;
    validate(state, outcome, failure_kind)?;
    let completed_bytes = event
        .transferred_bytes
        .as_ref()
        .map_or(Ok(0), |value| checked(value, "downloaded byte count"))?;
    let total_bytes = event
        .total_bytes
        .as_ref()
        .map(|value| checked(value, "download total"))
        .transpose()?
        .filter(|_| event.total_known != Some(false));
    let bytes_per_second = match &event.throughput_bytes_per_second {
        None => None,
        Some(Value::Number(number)) => number
            .as_f64()
            .filter(|speed| speed.is_finite() && *speed >= 0.)
            .map(|speed| speed.floor() as u64),
        Some(value) => Some(checked(value, "download throughput")?),
    };
    let nonempty = |value: &Option<String>| value.clone().filter(|text| !text.is_empty());
    Ok(DownloadStatus {
        id: event.transfer_id.clone(),
        scope_key: scope.transfer_key(),
        path: intent.path.clone(),
        destination: nonempty(&event.destination).or_else(|| Some(destination.to_owned())),
        kind: intent.kind,
        state,
        outcome,
        failure_kind,
        completed_bytes,
        total_bytes,
        bytes_per_second,
        eta_seconds: event.eta_seconds.filter(|eta| eta.is_finite()),
        error: nonempty(&event.error),
        cleanup_error: nonempty(&event.cleanup_error),
        cleanup_status,
    })
}

/// Decodes a download frame, or reports it as the failure it is rather than
/// casting it active.
pub fn decode_event(
    json: &str,
    intent: &DownloadIntent,
    destination: &str,
    scope: &FileScope,
) -> DownloadStatus {
    let event: WireDownloadEvent = serde_json::from_str(json).unwrap_or_default();
    map_event(&event, intent, destination, scope).unwrap_or_else(|error| {
        let id = if event.transfer_id.is_empty() {
            uuid::Uuid::new_v4().to_string()
        } else {
            event.transfer_id.clone()
        };
        DownloadStatus::refused(
            id,
            scope.transfer_key(),
            intent,
            Some(destination.to_owned()),
            error,
        )
    })
}

/// The downloads of one live connection. A transfer outlives pane, root and
/// session selection, and when its connection is replaced whatever is still in
/// flight has lost what would tell it how it ended. Port of
/// `useConnectionTransfers`.
#[derive(Default)]
pub struct DownloadList {
    connection_key: String,
    pub items: Vec<DownloadStatus>,
}

impl DownloadList {
    /// A first connection adopts what is there; a replacement retires it.
    /// Whether anything was retired.
    pub fn set_connection(&mut self, key: &str) -> bool {
        if self.connection_key == key {
            return false;
        }
        let mut retired = false;
        if !self.connection_key.is_empty() {
            for item in &mut self.items {
                if item.state.is_terminal() {
                    continue;
                }
                item.outcome = Some(if item.state == TransferState::Verifying {
                    TransferOutcome::Unknown
                } else {
                    TransferOutcome::NotPublished
                });
                item.state = TransferState::Failed;
                retired = true;
                item.failure_kind = Some(TransferFailureKind::StaleScope);
                item.error = Some(
                    "Transfer scope was replaced before its authoritative terminal event arrived."
                        .into(),
                );
            }
        }
        self.connection_key = key.to_owned();
        retired
    }

    pub fn record(&mut self, status: DownloadStatus) {
        if status.scope_key != self.connection_key {
            return;
        }
        match self.items.iter().position(|item| item.id == status.id) {
            Some(index) => {
                let current = self.items[index].clone();
                self.items[index] = current.merged(status);
            }
            None => {
                self.items.insert(0, status);
                self.items.truncate(MAX_LISTED);
            }
        }
    }

    pub fn clear_finished(&mut self) {
        self.items.retain(|item| !item.state.is_terminal());
    }

    pub fn get(&self, id: &str) -> Option<&DownloadStatus> {
        self.items.iter().find(|item| item.id == id)
    }
}

/// The download whose outcome the status channel still owes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ActiveDownload {
    pub id: String,
    pub path: String,
    pub banner: String,
}

#[derive(Debug, PartialEq, Eq)]
pub struct Reconciled {
    pub status: String,
    pub active: Option<ActiveDownload>,
    /// A finished file the user can open, and the message announcing it.
    pub completion: Option<String>,
}

/// What the status channel says once the active download ends. Only a newer
/// status worth a notice wins: routine chatter ("Live" after every snapshot)
/// replaced the banner long before most downloads finish. Port of
/// `reconcileDownloadStatus`.
pub fn reconcile_status(
    current: &str,
    active: Option<&ActiveDownload>,
    list: &[DownloadStatus],
) -> Reconciled {
    let unchanged = |active: Option<&ActiveDownload>| Reconciled {
        status: current.to_owned(),
        active: active.cloned(),
        completion: None,
    };
    let Some(active) = active else {
        return unchanged(None);
    };
    let Some(transfer) = list
        .iter()
        .find(|item| item.id == active.id)
        .filter(|item| item.state.is_terminal())
    else {
        return unchanged(Some(active));
    };
    if current != active.banner && notices::notice_for_status(current).is_some() {
        return unchanged(None);
    }
    if transfer.state == TransferState::Completed
        && let Some(destination) = &transfer.destination
    {
        // The local file the user now has, not the remote source.
        let message = format!("Download complete: {destination}");
        return Reconciled {
            status: message.clone(),
            active: None,
            completion: Some(destination.clone()),
        };
    }
    Reconciled {
        status: format!("Download {}: {}", state_word(transfer.state), active.path),
        active: None,
        completion: None,
    }
}

pub fn state_word(state: TransferState) -> &'static str {
    match state {
        TransferState::Queued => "queued",
        TransferState::Preflighting => "preflighting",
        TransferState::Running => "running",
        TransferState::Verifying => "verifying",
        TransferState::Completed => "completed",
        TransferState::Cancelled => "cancelled",
        TransferState::Failed => "failed",
    }
}

/// The save panel's starting name. A folder downloads as a tar archive.
pub fn suggested_name(intent: &DownloadIntent) -> String {
    let trimmed = intent.path.trim_end_matches('/');
    let name = trimmed
        .rsplit('/')
        .next()
        .filter(|name| !name.is_empty())
        .unwrap_or("download");
    if intent.kind == DownloadKind::Folder && !name.ends_with(".tar") {
        format!("{name}.tar")
    } else {
        name.to_owned()
    }
}

/// Only macOS has a Finder.
pub fn reveal_label() -> &'static str {
    if cfg!(target_os = "macos") {
        "Show in Finder"
    } else {
        "Show in folder"
    }
}
