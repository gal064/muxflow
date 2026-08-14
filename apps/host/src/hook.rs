use std::{
    fs::{self, OpenOptions},
    io::{Read, Write},
    os::unix::fs::OpenOptionsExt,
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, bail};
use prost::Message;
use tmux_agent_protocol::{envelope, read_frame, v1, write_frame};
use tokio::{
    net::UnixStream,
    time::{Duration, timeout},
};

const MAX_HOOK_BYTES: usize = 256 * 1024;

pub(crate) async fn run(arguments: Vec<String>) -> anyhow::Result<()> {
    let adapter = parse_adapter(&arguments)?;
    let mut payload = Vec::new();
    std::io::stdin()
        .take((MAX_HOOK_BYTES + 1) as u64)
        .read_to_end(&mut payload)?;
    if payload.len() > MAX_HOOK_BYTES {
        bail!("hook payload exceeds the {MAX_HOOK_BYTES}-byte limit");
    }
    // Validate before writing a fallback mailbox. Vendor payload stays private
    // to the host and never traverses a desktop/public listener as raw JSON.
    let value: serde_json::Value = serde_json::from_slice(&payload).context("parse hook JSON")?;
    if !value.is_object() {
        bail!("hook payload must be a JSON object");
    }
    let pane_id = std::env::var("TMUX_PANE").context("TMUX_PANE is unavailable")?;
    validate_pane_id(&pane_id)?;
    let origin_server_identity =
        crate::service::snapshot::inherited_server_identity().unwrap_or_default();
    let now = now_millis();
    let event = build_event(adapter, payload, &pane_id, &origin_server_identity, now)?;
    let socket = crate::paths::default_socket_path();
    match send(&socket, &event).await {
        Ok(()) => Ok(()),
        Err(error) if is_connection_error(&error) => {
            persist_latest_fallback(&event)?;
            Ok(())
        }
        Err(error) => Err(error),
    }
}

/// `hook status|install|uninstall` — the same merge-only installer the desktop
/// drives, reachable without a UI.
///
/// This exists so the installer can be exercised against a fixture copied from
/// a real machine's configuration: `--home` relocates every adapter's file and
/// `--settings-path` relocates one adapter's, which is what keeps QA off a real
/// `~/.claude` while still testing the real merge.
pub(crate) fn manage(verb: &str, arguments: Vec<String>) -> anyhow::Result<()> {
    let home = flag(&arguments, "--home").map(std::path::PathBuf::from);
    let settings_path = flag(&arguments, "--settings-path").map(std::path::PathBuf::from);
    let adapter_id = flag(&arguments, "--adapter");
    let selected = adapter_id
        .as_deref()
        .map(|id| crate::service::agents::adapters::by_id(id).context("unsupported hook adapter"))
        .transpose()?;
    let config_override = match (settings_path, selected) {
        (Some(path), Some(adapter)) => Some((adapter.id(), path)),
        (Some(_), None) => {
            bail!("--settings-path names one adapter's configuration and requires --adapter")
        }
        (None, _) => None,
    };
    let manager = crate::service::agents::HookManager::with_overrides(home, config_override)?;
    let action = match verb {
        "status" => None,
        "install" => Some(v1::HookManagementAction::Install),
        "uninstall" => Some(v1::HookManagementAction::Uninstall),
        _ => bail!("usage: tmux-ide-host hook <ingest|status|install|uninstall>"),
    };
    let observed = manager.wiring();
    let mut report = Vec::new();
    let mut failed = false;
    if let Some(action) = action {
        for entry in &observed {
            // Named explicitly, or chosen for us. Choosing for us means
            // skipping an agent that is not on this host at all — installing
            // there creates a configuration directory and file for a tool the
            // user does not use — and skipping one whose configuration could
            // not be read, because writing over what nobody could parse is how
            // unrelated hooks get lost. `--adapter` overrides the first;
            // nothing overrides the second.
            let explicit = adapter_id.as_deref() == Some(entry.adapter_id);
            let skip = match entry.state {
                v1::AgentHookWiring::Unavailable => Some("configuration could not be read"),
                v1::AgentHookWiring::Absent if !explicit => Some("agent is not installed here"),
                _ if adapter_id.is_some() && !explicit => Some("not selected"),
                _ => None,
            };
            if let Some(reason) = skip {
                report.push(serde_json::json!({
                    "adapterId": entry.adapter_id,
                    "configPath": entry.config_path,
                    "skipped": reason,
                }));
                continue;
            }
            let adapter = crate::service::agents::adapters::by_id(entry.adapter_id)
                .context("agent adapter is required")?;
            // Every adapter is reported even when an earlier one failed: a
            // merge-only installer whose pitch is "you can see exactly what
            // changed" must not exit silently having already written a file.
            match apply_one(&manager, adapter.legacy_kind(), action) {
                Ok(value) => report.push(value),
                Err(error) => {
                    failed = true;
                    report.push(serde_json::json!({
                        "adapterId": entry.adapter_id,
                        "configPath": entry.config_path,
                        "error": error.to_string(),
                    }));
                }
            }
        }
    }
    // Re-read: what the wiring is *after* whatever just happened is the
    // useful answer, and the pre-action observation above was only a plan.
    let wiring: Vec<_> = manager
        .wiring()
        .into_iter()
        .filter(|entry| {
            adapter_id
                .as_deref()
                .is_none_or(|id| id == entry.adapter_id)
        })
        .map(|entry| {
            serde_json::json!({
                "adapterId": entry.adapter_id,
                "configPath": entry.config_path,
                "wiring": wiring_label(entry.state),
                "detail": entry.detail,
            })
        })
        .collect();
    println!(
        "{}",
        serde_json::json!({
            "action": verb,
            "applied": report,
            "adapters": wiring,
        })
    );
    if failed {
        bail!("one or more adapters could not be updated; see the reported result");
    }
    Ok(())
}

fn apply_one(
    manager: &crate::service::agents::HookManager,
    adapter: v1::AgentAdapterKind,
    action: v1::HookManagementAction,
) -> anyhow::Result<serde_json::Value> {
    let review = manager.review(adapter, action)?;
    // `already_current` is the installer's own idempotence answer, so a second
    // run reports "unchanged" rather than rewriting a file and claiming it did
    // something.
    let changed = !review.already_current;
    if changed {
        manager.apply(adapter, action, &review.confirmation_token)?;
    }
    Ok(serde_json::json!({
        "adapterId": review.adapter_id,
        "configPath": review.config_path,
        "backupPath": review.backup_path,
        "changed": changed,
    }))
}

fn wiring_label(state: v1::AgentHookWiring) -> &'static str {
    match state {
        v1::AgentHookWiring::Wired => "wired",
        v1::AgentHookWiring::Partial => "partial",
        v1::AgentHookWiring::NotWired => "notWired",
        v1::AgentHookWiring::Absent => "absent",
        v1::AgentHookWiring::Unavailable => "unavailable",
        v1::AgentHookWiring::Unspecified => "unspecified",
    }
}

fn flag(arguments: &[String], name: &str) -> Option<String> {
    arguments
        .windows(2)
        .find(|pair| pair[0] == name)
        .map(|pair| pair[1].clone())
}

fn build_event(
    adapter: v1::AgentAdapterKind,
    payload: Vec<u8>,
    pane_id: &str,
    origin_server_identity: &str,
    now: i64,
) -> anyhow::Result<v1::AgentHookEvent> {
    let value: serde_json::Value = serde_json::from_slice(&payload).context("parse hook JSON")?;
    let native_session_id = string_field(&value, &["session_id", "sessionId"]);
    // Neither currently supported adapter documents a stable hook sequence.
    // Wall time and incidental "generation" fields are not causal ordering.
    let source_generation = 0;
    let source_event_id = string_field(&value, &["event_id", "eventId", "hook_event_id"]);
    let source_event_id = if source_event_id.is_empty() {
        uuid::Uuid::new_v4().to_string()
    } else {
        source_event_id
    };
    let event_name = string_field(&value, &["hook_event_name", "hookEventName", "event"]);
    let notification_type = string_field(&value, &["notification_type", "notificationType"]);
    let mut normalized = serde_json::Map::new();
    normalized.insert("hook_event_name".into(), event_name.into());
    if !native_session_id.is_empty() {
        normalized.insert("session_id".into(), native_session_id.clone().into());
    }
    if !notification_type.is_empty() {
        normalized.insert("notification_type".into(), notification_type.into());
    }
    for field in ["background_tasks", "session_crons"] {
        if value.get(field).is_some_and(nonempty_json) {
            normalized.insert(field.into(), serde_json::json!([true]));
        }
    }
    Ok(v1::AgentHookEvent {
        adapter: adapter.into(),
        adapter_id: crate::service::agents::adapters::adapter(adapter)
            .map(|adapter| adapter.id().to_owned())
            .unwrap_or_default(),
        source_event_id,
        source_generation,
        native_session_id,
        pane_id: pane_id.into(),
        payload_json: serde_json::to_vec(&normalized)?,
        occurred_at_unix_millis: now,
        source_sequence_authoritative: false,
        origin_server_identity: origin_server_identity.into(),
    })
}

fn nonempty_json(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::Array(value) => !value.is_empty(),
        serde_json::Value::Object(value) => !value.is_empty(),
        serde_json::Value::String(value) => !value.is_empty(),
        serde_json::Value::Number(value) => value.as_u64().is_some_and(|value| value > 0),
        serde_json::Value::Bool(value) => *value,
        _ => false,
    }
}

async fn send(socket: &Path, event: &v1::AgentHookEvent) -> anyhow::Result<()> {
    let mut stream = timeout(Duration::from_secs(1), UnixStream::connect(socket))
        .await
        .context("private daemon connection timed out")??;
    write_frame(
        &mut stream,
        &envelope(
            1,
            0,
            v1::envelope::Payload::ClientHello(v1::ClientHello {
                desktop_version: tmux_agent_protocol::HELPER_VERSION.into(),
                requested_capabilities: tmux_agent_protocol::CAP_AGENTS,
                expected_helper_version: tmux_agent_protocol::HELPER_VERSION.into(),
                bulk_connection: false,
                ..Default::default()
            }),
        ),
    )
    .await?;
    let hello = timeout(Duration::from_secs(1), read_frame(&mut stream))
        .await
        .context("private daemon handshake timed out")??
        .context("private daemon closed during handshake")?;
    let Some(v1::envelope::Payload::ServerHello(hello)) = hello.payload else {
        bail!("private daemon returned an invalid handshake");
    };
    if hello.read_only || hello.capabilities & tmux_agent_protocol::CAP_AGENTS == 0 {
        bail!("private daemon does not accept this hook protocol version");
    }
    write_frame(
        &mut stream,
        &envelope(
            2,
            0,
            v1::envelope::Payload::Request(v1::Request {
                operation: v1::Operation::AgentHookIngest.into(),
                agent: Some(v1::AgentRequest {
                    hook_event: Some(event.clone()),
                    ..Default::default()
                }),
                ..Default::default()
            }),
        ),
    )
    .await?;
    loop {
        let frame = timeout(Duration::from_secs(2), read_frame(&mut stream))
            .await
            .context("hook acknowledgement timed out")??
            .context("private daemon closed before hook acknowledgement")?;
        if frame.request_id != 2 {
            continue;
        }
        let Some(v1::envelope::Payload::Response(response)) = frame.payload else {
            bail!("private daemon returned an invalid hook acknowledgement");
        };
        if !response.ok {
            bail!("private daemon rejected hook: {}", response.display_message);
        }
        return Ok(());
    }
}

/// Events a stopped daemon could not be told about, kept in order.
///
/// This used to be one file per pane, overwritten by each event, and that
/// discarded exactly the sequence the daemon needs. A turn that starts and then
/// blocks while the daemon is down left only the block behind — and the
/// daemon's own rule that a late tool event may not revive a finished turn then
/// correctly ignored it, because the prompt that opened the new turn had been
/// overwritten. The user closed the app mid-turn, the agent asked for
/// permission, and the app came back showing the *previous* turn's result.
///
/// Bounded rather than unbounded: a mailbox that nothing ever drains must not
/// grow without limit, so the oldest events are dropped once a pane has this
/// many waiting. Dropping the oldest keeps the tail, which is the part that
/// describes where the agent ended up.
const MAX_FALLBACK_PER_PANE: usize = 32;

fn persist_latest_fallback(event: &v1::AgentHookEvent) -> anyhow::Result<()> {
    let runtime = crate::paths::default_runtime_dir();
    crate::paths::prepare_runtime_dir(&runtime)?;
    let pane = event.pane_id.trim_start_matches('%');
    let adapter = crate::service::agents::adapters::adapter(
        v1::AgentAdapterKind::try_from(event.adapter).unwrap_or_default(),
    )
    .context("unsupported hook adapter")?
    .id();
    let prefix = format!("hook-fallback-{adapter}-{pane}-");
    prune_fallbacks(&runtime, &prefix);
    // Fixed-width nanoseconds first, so the file name sorts chronologically and
    // the daemon can replay the sequence without opening anything; the random
    // suffix separates two hooks that fired in the same nanosecond, which are
    // concurrent and have no order to preserve anyway.
    let path = runtime.join(format!(
        "{prefix}{:020}-{}.pb",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos(),
        uuid::Uuid::new_v4().simple()
    ));
    let temporary = runtime.join(format!(".hook-fallback-{}.tmp", uuid::Uuid::new_v4()));
    let result = (|| -> anyhow::Result<()> {
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(&temporary)?;
        file.write_all(&event.encode_to_vec())?;
        file.sync_all()?;
        fs::rename(&temporary, path)?;
        fs::File::open(&runtime)?.sync_all()?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(temporary);
    }
    result
}

/// Drop the oldest waiting events for this pane once the mailbox is full.
///
/// Best effort by design: a mailbox that cannot be pruned is not a reason to
/// lose the event that is being written now.
fn prune_fallbacks(runtime: &Path, prefix: &str) {
    let Ok(entries) = fs::read_dir(runtime) else {
        return;
    };
    let mut existing: Vec<_> = entries
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().to_str()?.to_owned();
            (name.starts_with(prefix) && name.ends_with(".pb")).then(|| (name, entry.path()))
        })
        .collect();
    if existing.len() < MAX_FALLBACK_PER_PANE {
        return;
    }
    existing.sort_by(|left, right| left.0.cmp(&right.0));
    for (_, path) in existing
        .iter()
        .take(existing.len() + 1 - MAX_FALLBACK_PER_PANE)
    {
        let _ = fs::remove_file(path);
    }
}

fn parse_adapter(arguments: &[String]) -> anyhow::Result<v1::AgentAdapterKind> {
    let value =
        flag(arguments, "--adapter").context("hook ingest requires --adapter codex|claude-code")?;
    crate::service::agents::adapters::by_id(&value)
        .map(|adapter| adapter.legacy_kind())
        .ok_or_else(|| anyhow::anyhow!("unsupported hook adapter"))
}

fn validate_pane_id(value: &str) -> anyhow::Result<()> {
    if value.strip_prefix('%').is_some_and(|digits| {
        !digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_digit())
    }) {
        Ok(())
    } else {
        bail!("TMUX_PANE must be an exact tmux % pane ID")
    }
}

fn string_field(value: &serde_json::Value, names: &[&str]) -> String {
    names
        .iter()
        .find_map(|name| value.get(name).and_then(serde_json::Value::as_str))
        .unwrap_or_default()
        .to_owned()
}

fn now_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(i64::MAX)
}

fn is_connection_error(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        cause
            .downcast_ref::<tokio::time::error::Elapsed>()
            .is_some()
            || cause.downcast_ref::<std::io::Error>().is_some_and(|error| {
                matches!(
                    error.kind(),
                    std::io::ErrorKind::NotFound
                        | std::io::ErrorKind::ConnectionRefused
                        | std::io::ErrorKind::ConnectionReset
                        | std::io::ErrorKind::BrokenPipe
                )
            })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adapters_and_pane_identity_are_strict() {
        assert_eq!(
            parse_adapter(&["--adapter".into(), "codex".into()]).unwrap(),
            v1::AgentAdapterKind::Codex
        );
        assert!(parse_adapter(&["--adapter".into(), "other".into()]).is_err());
        assert!(validate_pane_id("%12").is_ok());
        assert!(validate_pane_id("%12;bad").is_err());
    }

    #[test]
    fn hook_events_use_the_exact_pane_and_are_unsequenced() {
        let event = build_event(
            v1::AgentAdapterKind::Codex,
            br#"{"hook_event_name":"Stop","generation":99}"#.to_vec(),
            "%12",
            "tmux:server-a",
            7,
        )
        .unwrap();
        assert_eq!(event.pane_id, "%12");
        assert_eq!(event.origin_server_identity, "tmux:server-a");
        assert_eq!(event.source_generation, 0);
        assert!(!event.source_sequence_authoritative);
    }

    #[test]
    fn fallback_envelope_contains_only_normalized_lifecycle_fields() {
        let event = build_event(
            v1::AgentAdapterKind::ClaudeCode,
            br#"{"hook_event_name":"Stop","session_id":"s","prompt":"private prompt","api_token":"secret","background_tasks":[{"command":"private"}]}"#.to_vec(),
            "%12",
            "tmux:server-a",
            7,
        )
        .unwrap();
        let payload = String::from_utf8(event.payload_json).unwrap();
        assert!(payload.contains("background_tasks"));
        assert!(!payload.contains("private"));
        assert!(!payload.contains("secret"));
        assert!(!payload.contains("prompt"));
    }
}
