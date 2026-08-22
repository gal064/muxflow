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
    let Some(pane_id) = pane_for_hook(std::env::var_os("TMUX_PANE"))? else {
        return Ok(());
    };
    let origin_server_identity =
        crate::service::snapshot::inherited_server_identity().unwrap_or_default();
    let now = now_millis();
    let event = build_event(adapter, payload, &pane_id, &origin_server_identity, now)?;
    deliver(&crate::paths::runtime_dir_candidates(), &event).await
}

/// Hand the event to whichever daemon is actually running.
///
/// The candidate list exists because "the runtime directory" is not a property
/// of the machine but of the environment a process happened to inherit, and a
/// hook inherits a different one from the daemon (see `paths::record_runtime_dir`).
/// Only a connection error moves on to the next candidate: a daemon that
/// answered and then rejected the event is the daemon, and retrying the same
/// event against another directory would either duplicate it or hide the
/// rejection.
async fn deliver(
    candidates: &[std::path::PathBuf],
    event: &v1::AgentHookEvent,
) -> anyhow::Result<()> {
    for runtime in candidates {
        let socket = runtime.join("host.sock");
        if !socket.exists() {
            continue;
        }
        match send(&socket, event).await {
            Ok(v1::HookIngestDisposition::Applied | v1::HookIngestDisposition::Discarded) => {
                return Ok(());
            }
            Ok(v1::HookIngestDisposition::Retryable | v1::HookIngestDisposition::Unspecified) => {
                // The daemon answered, so do not probe another candidate and
                // risk delivering twice. One rejection produces one durable
                // mailbox entry for the daemon to replay later.
                return persist_latest_fallback(runtime, event);
            }
            Err(HookDeliveryFailure::PreDelivery(error)) => {
                drop(error);
                continue;
            }
            Err(HookDeliveryFailure::Ambiguous(error)) => {
                // The request may already have committed. Never probe another
                // daemon after that boundary; persist beside the one that may
                // have applied it and let source-ID dedupe reconcile replay.
                drop(error);
                return persist_latest_fallback(runtime, event);
            }
        }
    }
    persist_latest_fallback(&crate::paths::fallback_runtime_dir(candidates), event)
}

#[derive(Debug)]
enum HookDeliveryFailure {
    /// No ingest request bytes were attempted; another runtime candidate is
    /// still safe to try.
    PreDelivery(anyhow::Error),
    /// Request delivery began, so absence of an acknowledgement is not proof
    /// that the daemon did not apply it.
    Ambiguous(anyhow::Error),
}

/// `hook status|install|uninstall` — the same merge-only installer the desktop
/// drives, reachable without a UI.
///
/// This exists so the installer can be exercised against a fixture copied from
/// a real machine's configuration: `--home` relocates every adapter's file and
/// `--settings-path` relocates one adapter's, which is what keeps QA off a real
/// `~/.claude` while still testing the real merge.
pub(crate) fn manage(verb: &str, arguments: Vec<String>) -> anyhow::Result<()> {
    let options = Options::parse(&arguments);
    let home = options.home.clone().map(std::path::PathBuf::from);
    let settings_path = options.settings_path.clone().map(std::path::PathBuf::from);
    let adapter_id = options.adapter.clone();
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
        _ => bail!("usage: muxflow-host hook <ingest|status|install|uninstall>"),
    };
    // Consent, for the one caller that has no user interface to ask through.
    //
    // The desktop will not write a host's agent configuration without a
    // recorded answer for that host; this command had no notion of consent at
    // all, so anything that could run it — a script, an agent, a paste from a
    // README — rewrote the operator's real `~/.claude` and `~/.codex` silently.
    // `--yes` is what makes the answer explicit and, in a shell history, a
    // record.
    //
    // The question is which files this run would write, never which flags it
    // carries: `--home "$HOME"` and `--settings-path ~/.claude/settings.json`
    // are both redirections and both land on exactly the files an unredirected
    // run would. So the paths are compared against the ones this operator's own
    // environment resolves, which is also what leaves every fixture lane — all
    // of which redirect somewhere else — needing no answer.
    if action.is_some() && !options.confirmed {
        // `?`, never a default. A gate that permits when it cannot work out
        // what it is protecting is not a gate.
        let mine = crate::service::agents::HookManager::with_overrides(None, None)
            .context("resolve your own agent configuration to confirm this would not change it")?
            .wiring()
            .into_iter()
            .map(|(_, entry)| entry.config_path)
            .collect::<std::collections::HashSet<_>>();
        if let Some((_, entry)) = manager
            .wiring()
            .into_iter()
            .find(|(_, entry)| mine.contains(&entry.config_path))
        {
            bail!(
                "hook {verb} would change your own agent configuration at {}. \
                 Re-run with --yes to confirm, or point --home/--settings-path at a copy.",
                entry.config_path.display()
            );
        }
    }
    let mut report = Vec::new();
    let mut failed = false;
    if let Some(action) = action {
        for (adapter, entry) in manager.wiring() {
            // The two verbs ask opposite questions and had been sharing one
            // predicate. Install asks "would this act here" — `invites_setup`,
            // which excludes an agent that is not on the host, because
            // installing there creates a configuration directory and file for
            // a tool the user does not use. Uninstall asks "do we own anything
            // here", and a `Wired` adapter answers no to the first and yes to
            // the second: unscoped `hook uninstall` removed nothing at all and
            // reported that the agents were not installed.
            //
            // `--adapter` overrides absence, since an operator naming an
            // adapter has said something the probe cannot. Nothing overrides an
            // unreadable configuration: writing over what nobody could parse is
            // how unrelated hooks get lost.
            let explicit = adapter_id.as_deref() == Some(entry.adapter_id);
            let acts_here = match action {
                v1::HookManagementAction::Uninstall => {
                    entry.state == v1::AgentHookWiring::Wired
                        || entry.state == v1::AgentHookWiring::Partial
                }
                _ => entry.state.invites_setup(),
            };
            let skip = if adapter_id.is_some() && !explicit {
                Some("not selected")
            } else if entry.state == v1::AgentHookWiring::Unavailable {
                Some("configuration could not be read")
            } else if !acts_here && !explicit {
                Some(match action {
                    v1::HookManagementAction::Uninstall => "nothing of ours is installed here",
                    _ => "agent is not installed here",
                })
            } else {
                None
            };
            if let Some(reason) = skip {
                report.push(serde_json::json!({
                    "adapterId": entry.adapter_id,
                    "configPath": entry.config_path,
                    "skipped": reason,
                }));
                continue;
            }
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
    // useful answer, and the observation above was only a plan.
    let wiring: Vec<_> = manager
        .wiring()
        .into_iter()
        .map(|(_, entry)| entry)
        .filter(|entry| {
            adapter_id
                .as_deref()
                .is_none_or(|id| id == entry.adapter_id)
        })
        .map(|entry| {
            serde_json::json!({
                "adapterId": entry.adapter_id,
                "configPath": entry.config_path,
                "wiring": entry.state.label(),
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

/// Everything this command reads from its arguments, parsed once.
///
/// One traversal, because there were two and they disagreed. A `windows(2)`
/// search matches a flag anywhere, including where it is another flag's value;
/// a positional walk does not. With both models present, `--adapter --home /x`
/// meant different things to the code that decides *where* to write and the
/// code that decides *whether* it may — on the one command that writes the
/// user's configuration files.
#[derive(Default)]
struct Options {
    adapter: Option<String>,
    home: Option<String>,
    settings_path: Option<String>,
    confirmed: bool,
}

impl Options {
    fn parse(arguments: &[String]) -> Self {
        let mut options = Self::default();
        let mut rest = arguments.iter();
        while let Some(argument) = rest.next() {
            let field = match argument.as_str() {
                "--adapter" => &mut options.adapter,
                "--home" => &mut options.home,
                "--settings-path" => &mut options.settings_path,
                "--yes" => {
                    options.confirmed = true;
                    continue;
                }
                _ => continue,
            };
            // A flag whose value is missing stays unset rather than swallowing
            // the next flag, and the value is never re-read as one.
            *field = rest.next().cloned();
        }
        options
    }
}

pub(crate) fn build_event(
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
    if let Some(key) = approval_key(&value) {
        // Correlate a PermissionRequest with the PostToolUse that proves that
        // exact tool was approved and completed. The vendor payload can carry
        // commands, paths and prompts, so only this one-way digest crosses the
        // hook boundary or reaches the durable agent store.
        normalized.insert("approval_key".into(), key.into());
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

fn approval_key(value: &serde_json::Value) -> Option<String> {
    let turn_id = string_field(value, &["turn_id", "turnId"]);
    let tool_name = string_field(value, &["tool_name", "toolName"]);
    let tool_input = value.get("tool_input").or_else(|| value.get("toolInput"))?;
    if turn_id.is_empty() || tool_name.is_empty() {
        return None;
    }
    let mut hasher = blake3::Hasher::new();
    for part in [turn_id.as_bytes(), tool_name.as_bytes()] {
        hasher.update(&(part.len() as u64).to_le_bytes());
        hasher.update(part);
    }
    // Bash PermissionRequest adds request-only `description` metadata for
    // justifications and managed-network approvals. PostToolUse does not. The
    // command is the stable tool identity shared by both contracts; hashing
    // the whole permission input would strand the pending request.
    let tool_identity = if tool_name == "Bash" {
        tool_input.get("command").unwrap_or(tool_input)
    } else {
        tool_input
    };
    let encoded = serde_json::to_vec(&canonical_json(tool_identity)).ok()?;
    hasher.update(&(encoded.len() as u64).to_le_bytes());
    hasher.update(&encoded);
    Some(format!("v1:{}", hasher.finalize().to_hex()))
}

fn canonical_json(value: &serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Array(values) => {
            serde_json::Value::Array(values.iter().map(canonical_json).collect())
        }
        serde_json::Value::Object(values) => {
            let mut entries: Vec<_> = values.iter().collect();
            entries.sort_unstable_by_key(|(key, _)| *key);
            serde_json::Value::Object(
                entries
                    .into_iter()
                    .map(|(key, value)| (key.clone(), canonical_json(value)))
                    .collect(),
            )
        }
        value => value.clone(),
    }
}
async fn send(
    socket: &Path,
    event: &v1::AgentHookEvent,
) -> Result<v1::HookIngestDisposition, HookDeliveryFailure> {
    let mut stream = connect_and_handshake(socket)
        .await
        .map_err(HookDeliveryFailure::PreDelivery)?;
    send_request_and_wait(&mut stream, event)
        .await
        .map_err(HookDeliveryFailure::Ambiguous)
}

async fn connect_and_handshake(socket: &Path) -> anyhow::Result<UnixStream> {
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
    Ok(stream)
}

async fn send_request_and_wait(
    stream: &mut UnixStream,
    event: &v1::AgentHookEvent,
) -> anyhow::Result<v1::HookIngestDisposition> {
    write_frame(
        stream,
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
        let frame = timeout(Duration::from_secs(2), read_frame(stream))
            .await
            .context("hook acknowledgement timed out")??
            .context("private daemon closed before hook acknowledgement")?;
        if frame.request_id != 2 {
            continue;
        }
        let Some(v1::envelope::Payload::Response(response)) = frame.payload else {
            bail!("private daemon returned an invalid hook acknowledgement");
        };
        let disposition = v1::HookIngestDisposition::try_from(response.hook_ingest_disposition)
            .unwrap_or_default();
        if disposition != v1::HookIngestDisposition::Unspecified {
            return Ok(disposition);
        }
        bail!("private daemon omitted the typed hook-ingest disposition");
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

fn persist_latest_fallback(runtime: &Path, event: &v1::AgentHookEvent) -> anyhow::Result<()> {
    let _mailbox = crate::hook_mailbox::HookMailboxLock::acquire(runtime)?;
    let pane = event.pane_id.trim_start_matches('%');
    let adapter = crate::service::agents::adapters::adapter(
        v1::AgentAdapterKind::try_from(event.adapter).unwrap_or_default(),
    )
    .context("unsupported hook adapter")?
    .id();
    let prefix = format!("hook-fallback-{adapter}-{pane}-");
    prune_fallbacks(runtime, &prefix);
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
        fs::File::open(runtime)?.sync_all()?;
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
    let value = Options::parse(arguments)
        .adapter
        .context("hook ingest requires --adapter codex|claude-code")?;
    crate::service::agents::adapters::by_id(&value)
        .map(|adapter| adapter.legacy_kind())
        .ok_or_else(|| anyhow::anyhow!("unsupported hook adapter"))
}

/// Which pane this hook belongs to, or `None` when it belongs to no pane.
///
/// The managed hook line is installed once, into the agent's own configuration,
/// and that configuration follows the user everywhere the agent runs — including
/// a plain terminal with no tmux server in sight. There is legitimately nothing
/// to ingest there, and treating it as an error surfaced
/// `UserPromptSubmit hook error … TMUX_PANE is unavailable` on *every* prompt of
/// every such session.
///
/// Absent is the no-tmux case and is silent. Present-but-malformed is not: a
/// value that exists and is not a pane id means something in the environment
/// claims to be tmux and is wrong, which the operator has to be told about. That
/// includes a value that is not UTF-8 at all, which is why the caller passes
/// `var_os` — `var` reports it as absent and would have swallowed exactly the
/// misconfiguration this distinction exists to catch.
fn pane_for_hook(value: Option<std::ffi::OsString>) -> anyhow::Result<Option<String>> {
    let Some(value) = value else {
        return Ok(None);
    };
    let pane_id = value
        .to_str()
        .context("TMUX_PANE is not valid UTF-8 and cannot be a tmux pane ID")?;
    validate_pane_id(pane_id)?;
    Ok(Some(pane_id.to_owned()))
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

#[cfg(test)]
mod tests {
    use super::*;

    fn rejecting_daemon(
        runtime: &Path,
        disposition: v1::HookIngestDisposition,
    ) -> tokio::task::JoinHandle<()> {
        let listener = tokio::net::UnixListener::bind(runtime.join("host.sock")).unwrap();
        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let hello = read_frame(&mut stream).await.unwrap().unwrap();
            assert!(matches!(
                hello.payload,
                Some(v1::envelope::Payload::ClientHello(_))
            ));
            write_frame(
                &mut stream,
                &envelope(
                    1,
                    0,
                    v1::envelope::Payload::ServerHello(v1::ServerHello {
                        capabilities: tmux_agent_protocol::CAP_AGENTS,
                        ..Default::default()
                    }),
                ),
            )
            .await
            .unwrap();
            let request = read_frame(&mut stream).await.unwrap().unwrap();
            assert_eq!(request.request_id, 2);
            write_frame(
                &mut stream,
                &envelope(
                    2,
                    0,
                    v1::envelope::Payload::Response(v1::Response {
                        ok: false,
                        display_message: "redacted injected rejection".into(),
                        hook_ingest_disposition: disposition.into(),
                        ..Default::default()
                    }),
                ),
            )
            .await
            .unwrap();
        })
    }

    fn fallback_count(runtime: &Path) -> usize {
        fs::read_dir(runtime)
            .unwrap()
            .flatten()
            .filter(|entry| {
                entry
                    .file_name()
                    .to_str()
                    .is_some_and(|name| name.starts_with("hook-fallback-") && name.ends_with(".pb"))
            })
            .count()
    }

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

    /// Running the agent outside tmux is not a misconfiguration and must not
    /// look like one; running it with a broken `TMUX_PANE` still must.
    #[test]
    fn a_session_outside_tmux_has_no_pane_and_is_not_an_error() {
        use std::{ffi::OsString, os::unix::ffi::OsStringExt};

        assert_eq!(pane_for_hook(None).unwrap(), None);
        assert_eq!(
            pane_for_hook(Some("%12".into())).unwrap(),
            Some("%12".to_owned())
        );
        for malformed in ["", "%", "12", "%12;bad", "%1 2"] {
            assert!(
                pane_for_hook(Some(malformed.into())).is_err(),
                "a present but malformed TMUX_PANE ({malformed:?}) must still fail loudly"
            );
        }
        assert!(pane_for_hook(Some(OsString::from_vec(vec![0xff, 0xfe]))).is_err());
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
        // Only the fields lifecycle is decided from survive. Background work
        // is not one of them: a Stop ends the turn whatever is still running.
        assert!(!payload.contains("background_tasks"));
        assert!(!payload.contains("private"));
        assert!(!payload.contains("secret"));
        assert!(!payload.contains("prompt"));
    }

    #[test]
    fn approval_correlation_is_stable_and_keeps_vendor_input_private() {
        let build = |event: &str, tool_input: serde_json::Value| {
            build_event(
                v1::AgentAdapterKind::Codex,
                serde_json::to_vec(&serde_json::json!({
                    "hook_event_name": event,
                    "session_id": "session-private",
                    "turn_id": "turn-7",
                    "tool_name": "Bash",
                    "tool_input": tool_input,
                    "tool_use_id": "not-shared"
                }))
                .unwrap(),
                "%12",
                "tmux:server-a",
                7,
            )
            .unwrap()
        };
        let permission = build(
            "PermissionRequest",
            serde_json::json!({
                "description": "Allow network access to a private host",
                "command": "curl secret.example"
            }),
        );
        let matching = build(
            "PostToolUse",
            serde_json::json!({"command": "curl secret.example"}),
        );
        let unrelated = build("PostToolUse", serde_json::json!({"command": "pwd"}));
        let normalized = |event: &v1::AgentHookEvent| {
            serde_json::from_slice::<serde_json::Value>(&event.payload_json).unwrap()
        };

        let permission = normalized(&permission);
        let matching = normalized(&matching);
        let unrelated = normalized(&unrelated);
        assert_eq!(permission["approval_key"], matching["approval_key"]);
        assert_ne!(permission["approval_key"], unrelated["approval_key"]);
        let serialized = permission.to_string();
        for private in ["secret.example", "not-shared", "turn-7"] {
            assert!(!serialized.contains(private));
        }
    }

    #[tokio::test]
    async fn retryable_live_rejection_writes_exactly_one_fallback_event() {
        let runtime = std::env::temp_dir().join(format!(
            "ade-hr-{}",
            &uuid::Uuid::new_v4().simple().to_string()[..12]
        ));
        fs::create_dir_all(&runtime).unwrap();
        let server = rejecting_daemon(&runtime, v1::HookIngestDisposition::Retryable);
        let event = build_event(
            v1::AgentAdapterKind::Codex,
            br#"{"hook_event_name":"PermissionRequest","event_id":"retry-once"}"#.to_vec(),
            "%7",
            "server-a",
            7,
        )
        .unwrap();

        deliver(std::slice::from_ref(&runtime), &event)
            .await
            .unwrap();
        server.await.unwrap();
        assert_eq!(fallback_count(&runtime), 1);
        fs::remove_dir_all(runtime).unwrap();
    }

    #[tokio::test]
    async fn permanent_live_rejection_is_discarded_without_a_fallback_event() {
        let runtime = std::env::temp_dir().join(format!(
            "ade-hd-{}",
            &uuid::Uuid::new_v4().simple().to_string()[..12]
        ));
        fs::create_dir_all(&runtime).unwrap();
        let server = rejecting_daemon(&runtime, v1::HookIngestDisposition::Discarded);
        let event = build_event(
            v1::AgentAdapterKind::Codex,
            br#"{"hook_event_name":"PermissionRequest","event_id":"discard-once"}"#.to_vec(),
            "%7",
            "server-a",
            7,
        )
        .unwrap();

        deliver(std::slice::from_ref(&runtime), &event)
            .await
            .unwrap();
        server.await.unwrap();
        assert_eq!(fallback_count(&runtime), 0);
        fs::remove_dir_all(runtime).unwrap();
    }

    #[tokio::test]
    async fn ambiguous_ack_never_retries_a_second_daemon() {
        // A short root, not the default tempdir: the sockets below must stay
        // under the platform's 104/108-byte bind limit, and macOS puts the
        // default tempdir 50+ bytes deep under /var/folders.
        let root =
            std::path::PathBuf::from("/tmp").join(format!("ade-ha-{}", uuid::Uuid::new_v4()));
        let first = root.join("first");
        let second = root.join("second");
        fs::create_dir_all(&first).unwrap();
        fs::create_dir_all(&second).unwrap();
        let first_listener = tokio::net::UnixListener::bind(first.join("host.sock")).unwrap();
        let second_listener = tokio::net::UnixListener::bind(second.join("host.sock")).unwrap();
        let first_server = tokio::spawn(async move {
            let (mut stream, _) = first_listener.accept().await.unwrap();
            let _hello = read_frame(&mut stream).await.unwrap().unwrap();
            write_frame(
                &mut stream,
                &envelope(
                    1,
                    0,
                    v1::envelope::Payload::ServerHello(v1::ServerHello {
                        capabilities: tmux_agent_protocol::CAP_AGENTS,
                        ..Default::default()
                    }),
                ),
            )
            .await
            .unwrap();
            let request = read_frame(&mut stream).await.unwrap().unwrap();
            assert_eq!(request.request_id, 2);
            // Applied but acknowledgement lost.
        });
        let event = build_event(
            v1::AgentAdapterKind::Codex,
            br#"{"hook_event_name":"Stop","event_id":"ambiguous"}"#.to_vec(),
            "%7",
            "server-a",
            7,
        )
        .unwrap();
        deliver(&[first.clone(), second.clone()], &event)
            .await
            .unwrap();
        first_server.await.unwrap();
        assert!(
            timeout(Duration::from_millis(100), second_listener.accept())
                .await
                .is_err()
        );
        assert_eq!(fallback_count(&first), 1);
        assert_eq!(fallback_count(&second), 0);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn concurrent_fallback_publishers_preserve_the_exact_per_pane_bound() {
        let runtime = std::env::temp_dir().join(format!("ade-hb-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&runtime).unwrap();
        let event = build_event(
            v1::AgentAdapterKind::Codex,
            br#"{"hook_event_name":"Stop","event_id":"bounded"}"#.to_vec(),
            "%7",
            "server-a",
            7,
        )
        .unwrap();
        let threads = (0..64)
            .map(|_| {
                let runtime = runtime.clone();
                let event = event.clone();
                std::thread::spawn(move || persist_latest_fallback(&runtime, &event).unwrap())
            })
            .collect::<Vec<_>>();
        for thread in threads {
            thread.join().unwrap();
        }
        assert_eq!(fallback_count(&runtime), MAX_FALLBACK_PER_PANE);
        fs::remove_dir_all(runtime).unwrap();
    }
}
