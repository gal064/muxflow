use std::{
    fs,
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
};

use anyhow::Context;
use prost::Message;
use tmux_agent_protocol::v1;

use super::{AgentRuntime, HookIngestFailure, ingest::MAX_HOOK_BYTES, publish};

static INGEST_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum HookReplayDisposition {
    Applied,
    Discard,
    Retain,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(super) struct HookReplayReport {
    pub(super) applied: usize,
    pub(super) retained: usize,
}

/// Replay everything a stopped daemon was told about.
///
/// This daemon's own directory first, and then the one it last published —
/// which is where a hook leaves an event it could not deliver, so that the
/// daemon coming back finds it. Nothing beyond those two: sweeping consumes
/// and deletes, and a directory no daemon on this machine has claimed is not
/// this one's to empty (M13-E003 was fixed once by a scan that did exactly
/// that, and drained a developer's real mailbox from a test).
pub(crate) fn ingest() -> anyhow::Result<usize> {
    // Startup replay and pre-live drains share one consumer. Concurrent hook
    // connections may observe the same mailbox names, but only one is allowed
    // to apply/delete them at a time.
    let _guard = INGEST_LOCK.get_or_init(|| Mutex::new(())).lock().unwrap();
    let mut swept = vec![crate::paths::runtime_dir()];
    if let Some(published) = crate::paths::published_runtime_dir()
        && !swept.contains(&published)
    {
        swept.push(published);
    }
    consume_roots(&swept, |event| {
        match AgentRuntime::global().ingest_hook(&event) {
            Ok(agent_event) => {
                publish(agent_event);
                HookReplayDisposition::Applied
            }
            Err(HookIngestFailure::Duplicate | HookIngestFailure::Permanent(_)) => {
                HookReplayDisposition::Discard
            }
            Err(HookIngestFailure::Retryable(error)) => {
                // The mailbox records only a safe counter at its caller;
                // consume the internal cause here without logging paths or
                // configuration details from the persistence failure.
                drop(error);
                HookReplayDisposition::Retain
            }
        }
    })
}

pub(super) fn consume_roots(
    runtime_dirs: &[PathBuf],
    mut consume_event: impl FnMut(v1::AgentHookEvent) -> HookReplayDisposition,
) -> anyhow::Result<usize> {
    let mut applied = 0;
    for runtime in runtime_dirs {
        let report = consume(runtime, &mut consume_event)?;
        applied += report.applied;
        if report.retained > 0 {
            anyhow::bail!(
                "{} hook fallback event(s) remain queued for retry",
                report.retained
            );
        }
    }
    Ok(applied)
}

pub(super) fn consume(
    runtime_dir: &Path,
    mut consume: impl FnMut(v1::AgentHookEvent) -> HookReplayDisposition,
) -> anyhow::Result<HookReplayReport> {
    let entries = match fs::read_dir(runtime_dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(HookReplayReport::default());
        }
        Err(error) => return Err(error.into()),
    };
    // Sorted, because these are a sequence and not a set. The writer names each
    // file `hook-fallback-<adapter>-<pane>-<nanoseconds>-<random>.pb`, so
    // sorting by name orders each *pane's* events the way they happened, which
    // is the ordering that matters: a turn belongs to one pane, and replaying
    // its `UserPromptSubmit` after its `PermissionRequest` would have the
    // daemon apply the block and then discard it as a late event from a turn
    // that had already finished. Two different panes interleave arbitrarily,
    // and are genuinely independent.
    let mut waiting: Vec<_> = entries
        .flatten()
        .filter(|entry| {
            entry
                .file_name()
                .to_str()
                .is_some_and(|name| name.starts_with("hook-fallback-") && name.ends_with(".pb"))
        })
        .collect();
    waiting.sort_by_key(|entry| entry.file_name());
    let mut report = HookReplayReport::default();
    let waiting_count = waiting.len();
    for (index, entry) in waiting.into_iter().enumerate() {
        let Ok(metadata) = entry.metadata() else {
            // Do not overtake an input whose bytes could not be inspected.
            // Later names are later lifecycle facts for at least this pane,
            // and applying them first would make the retained event regress
            // state when it is eventually replayed.
            report.retained += waiting_count - index;
            break;
        };
        if !metadata.is_file() || metadata.len() > MAX_HOOK_BYTES as u64 + 4096 {
            if metadata.is_file() {
                let _ = fs::remove_file(entry.path());
            }
            continue;
        }
        let bytes = match fs::read(entry.path()).context("read hook fallback") {
            Ok(bytes) => bytes,
            Err(_) => {
                report.retained += waiting_count - index;
                break;
            }
        };
        let event = match v1::AgentHookEvent::decode(bytes.as_slice()) {
            Ok(event) => event,
            Err(_) => {
                // A malformed protobuf can never become valid on retry.
                let _ = fs::remove_file(entry.path());
                continue;
            }
        };
        match consume(event) {
            HookReplayDisposition::Applied => {
                report.applied += 1;
                let _ = fs::remove_file(entry.path());
            }
            HookReplayDisposition::Discard => {
                // Duplicate and permanently invalid input are idempotent: they
                // are acknowledged by deletion and never replayed forever.
                let _ = fs::remove_file(entry.path());
            }
            HookReplayDisposition::Retain => {
                // Stop the ordered replay here. This is conservative across
                // independent panes, but it guarantees no later transition
                // overtakes a retained one and the bounded mailbox keeps the
                // retry set finite.
                report.retained += waiting_count - index;
                break;
            }
        }
    }
    Ok(report)
}
