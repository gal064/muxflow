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
    let mut ingested = 0;
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if !name.starts_with("hook-fallback-") || !name.ends_with(".pb") {
            continue;
        }
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
