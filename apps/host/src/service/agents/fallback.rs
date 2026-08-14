use std::{fs, path::Path};

use anyhow::Context;
use prost::Message;
use tmux_agent_protocol::v1;

use super::{AgentRuntime, MAX_HOOK_BYTES, publish};

pub(crate) fn ingest() -> anyhow::Result<usize> {
    consume(&crate::paths::default_runtime_dir(), |event| {
        if let Ok(agent_event) = AgentRuntime::global().ingest_hook(&event) {
            publish(agent_event);
            true
        } else {
            false
        }
    })
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
    // file with fixed-width nanoseconds, so the file name orders the events the
    // way they happened; replaying a turn's `UserPromptSubmit` after its
    // `PermissionRequest` would have the daemon apply the block and then
    // discard it as a late event from a finished turn.
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
