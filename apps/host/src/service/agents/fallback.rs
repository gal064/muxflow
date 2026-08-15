use std::{fs, path::Path};

use anyhow::Context;
use prost::Message;
use tmux_agent_protocol::v1;

use super::{AgentRuntime, MAX_HOOK_BYTES, publish};

/// Replay everything a stopped daemon was told about, from every directory a
/// hook could have left it in.
///
/// One directory was not enough: a hook whose environment resolved a different
/// runtime directory from this daemon's wrote a perfectly good mailbox that
/// nothing swept (M13-E003). The candidate list is the same one the hook uses,
/// so the two ends agree by construction, and an explicit
/// `ADE_HOST_RUNTIME_DIR` still collapses it to exactly one directory — a
/// fixture daemon never reaches into a neighbouring one.
pub(crate) fn ingest() -> anyhow::Result<usize> {
    let mut ingested = 0;
    let mut failure = None;
    for runtime in crate::paths::runtime_dir_candidates() {
        match consume(&runtime, |event| {
            if let Ok(agent_event) = AgentRuntime::global().ingest_hook(&event) {
                publish(agent_event);
                true
            } else {
                false
            }
        }) {
            Ok(count) => ingested += count,
            // A directory this daemon cannot read is not a reason to drop the
            // events it already replayed from the ones it could.
            Err(error) => failure = failure.or(Some(error)),
        }
    }
    match failure {
        Some(error) if ingested == 0 => Err(error),
        _ => Ok(ingested),
    }
}

pub(super) fn consume(
    runtime_dir: &Path,
    mut consume: impl FnMut(v1::AgentHookEvent) -> bool,
) -> anyhow::Result<usize> {
    let entries = match fs::read_dir(runtime_dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
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
    let mut ingested = 0;
    for entry in waiting {
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        if !metadata.is_file() || metadata.len() > MAX_HOOK_BYTES as u64 + 4096 {
            if metadata.is_file() {
                let _ = fs::remove_file(entry.path());
            }
            continue;
        }
        let event = fs::read(entry.path())
            .context("read hook fallback")
            .and_then(|bytes| {
                v1::AgentHookEvent::decode(bytes.as_slice()).context("decode hook fallback")
            });
        if event.is_ok_and(&mut consume) {
            ingested += 1;
        }
        let _ = fs::remove_file(entry.path());
    }
    Ok(ingested)
}
