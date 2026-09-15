use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicU64},
};

use anyhow::{Context, bail};
use tmux_agent_protocol::v1;

use super::super::{
    SequencerControl,
    agents::{AgentRuntime, HookIngestFailure, HookManager, publish},
    filesystem::validate_root_token,
    snapshot::{server_identity, tmux_command},
};
use super::{response_error, send_response};

// tmux normalizes literal control characters in `-F` strings to `_`, so tabs
// cannot delimit placement IDs portably. Exact tmux IDs contain only their
// sigil and decimal digits, making `:` an unambiguous cross-version separator.
const PLACEMENT_FORMAT: &str = "#{session_id}:#{window_id}:#{pane_id}";

pub(super) async fn handle(
    request_id: u64,
    operation: v1::Operation,
    request: v1::Request,
    control_tx: &tokio::sync::mpsc::Sender<SequencerControl>,
    generation: &Arc<AtomicU64>,
    topology_baseline: &Arc<Mutex<Option<(tmux_control::TmuxSnapshot, String)>>>,
    cancellation: &AtomicBool,
) {
    let result = handle_inner(
        operation,
        request.agent.context("agent request payload is required"),
        generation,
        topology_baseline,
        cancellation,
    );
    send_response(
        control_tx,
        request_id,
        result.unwrap_or_else(|error| response_error("agent_request_failed", &error.to_string())),
    )
    .await;
}

fn handle_inner(
    operation: v1::Operation,
    request: anyhow::Result<v1::AgentRequest>,
    generation: &AtomicU64,
    topology_baseline: &Mutex<Option<(tmux_control::TmuxSnapshot, String)>>,
    cancellation: &AtomicBool,
) -> anyhow::Result<v1::Response> {
    if cancellation.load(std::sync::atomic::Ordering::Acquire) {
        bail!("agent request was cancelled");
    }
    let request = request?;
    let runtime = AgentRuntime::global();
    let mut response = v1::AgentResponse::default();
    let mut diagnostic_lines = Vec::new();
    match operation {
        v1::Operation::AgentDiagnostics => {
            diagnostic_lines = crate::diagnostics::lifecycle_diagnostic_lines();
        }
        v1::Operation::AgentSnapshot => {
            // No sweep here any more. The daemon's own maintenance pass runs
            // every two seconds whether or not anything is connected, so a
            // reconnecting desktop cannot be handed a state older than one
            // tick — and doing it here only ever covered the case where a
            // desktop was present to ask.
            response.snapshot = Some(runtime.snapshot());
        }
        v1::Operation::AgentMarkSeen => {
            let event = runtime.mark_seen(&request.agent_id, request.attention_generation)?;
            response.agent = event.agent.clone();
            publish(event);
        }
        v1::Operation::AgentHookIngest => {
            match runtime.ingest_live_hook(
                request
                    .hook_event
                    .as_ref()
                    .context("normalized hook envelope is required")?,
            ) {
                Ok(event) => {
                    response.agent = event.agent;
                }
                Err(failure) => {
                    let disposition = failure.disposition();
                    let message = match failure {
                        HookIngestFailure::Duplicate => "hook event was already handled",
                        HookIngestFailure::Superseded => {
                            "hook event belongs to a superseded pane session"
                        }
                        HookIngestFailure::Permanent(error) => {
                            drop(error);
                            "hook event was permanently rejected"
                        }
                        HookIngestFailure::Retryable(error) => {
                            drop(error);
                            "hook event could not be persisted; retry later"
                        }
                    };
                    let mut response = response_error("agent_hook_ingest_rejected", message);
                    response.hook_ingest_disposition = disposition.into();
                    return Ok(response);
                }
            }
        }
        v1::Operation::AgentHookManagement => {
            let adapter = super::super::agents::adapters::by_id(&request.adapter_id)
                .context("supported agent adapter is required")?
                .legacy_kind();
            let action =
                v1::HookManagementAction::try_from(request.hook_management).unwrap_or_default();
            let manager = HookManager::system_default()?;
            response.hook_plan = Some(if action == v1::HookManagementAction::Review {
                let target = v1::HookManagementAction::try_from(request.hook_management_target)
                    .unwrap_or_default();
                manager.review(adapter, target)?
            } else {
                if !request.confirmed {
                    bail!("hook configuration changes require explicit confirmation");
                }
                manager.apply(adapter, action, &request.confirmation_token)?
            });
        }
        v1::Operation::AgentHostNaming => {
            // Requires no confirmation token of its own: nothing is written to
            // disk, and the one-time host prompt that authorised it is the same
            // consent as the hook install. The desktop re-sends it on every
            // connect because a tmux server restart drops the hook — and sends
            // the uninstall alongside the file hooks' own, because without an
            // explicit removal this would keep renaming the user's windows
            // until their server happened to restart.
            let outcome = if v1::HookManagementAction::try_from(request.hook_management)
                .unwrap_or_default()
                == v1::HookManagementAction::Uninstall
            {
                super::super::tmux_config::remove_recommended_naming()?
            } else {
                super::super::tmux_config::apply_recommended_naming()?
            };
            response.host_naming = outcome.label().into();
        }
        v1::Operation::AgentAction => {
            let action = v1::AgentActionKind::try_from(request.action).unwrap_or_default();
            if action == v1::AgentActionKind::Rename {
                let event = runtime.rename(&request.agent_id, &request.display_name)?;
                response.agent = event.agent.clone();
                publish(event);
            } else {
                let (session_id, window_id, pane_id) =
                    launch_agent(&request, action, generation, topology_baseline)?;
                response.session_id = session_id;
                response.window_id = window_id;
                response.pane_id = pane_id;
            }
        }
        _ => bail!("unsupported agent operation"),
    }
    response.accepted_generation = response
        .snapshot
        .as_ref()
        .map(|snapshot| snapshot.accepted_generation)
        .or_else(|| response.agent.as_ref().map(|agent| agent.state_generation))
        .unwrap_or_else(|| runtime.snapshot().accepted_generation);
    Ok(v1::Response {
        ok: true,
        agent: Some(response),
        diagnostic_lines,
        hook_ingest_disposition: if operation == v1::Operation::AgentHookIngest {
            v1::HookIngestDisposition::Applied.into()
        } else {
            Default::default()
        },
        ..Default::default()
    })
}

fn launch_agent(
    request: &v1::AgentRequest,
    action: v1::AgentActionKind,
    generation: &AtomicU64,
    topology_baseline: &Mutex<Option<(tmux_control::TmuxSnapshot, String)>>,
) -> anyhow::Result<(String, String, String)> {
    if !matches!(
        action,
        v1::AgentActionKind::LaunchWindow
            | v1::AgentActionKind::LaunchSplit
            | v1::AgentActionKind::Resume
    ) {
        bail!("launch action is required");
    }
    validate_root_token(&request.active_root, &request.root_token)?;
    let identity = server_identity();
    if request.expected_server_identity != identity {
        bail!("tmux server identity changed; refresh before launching an agent");
    }
    let current_generation = generation.load(std::sync::atomic::Ordering::Acquire);
    if request.expected_topology_generation != current_generation {
        bail!("tmux topology generation changed; refresh before launching an agent");
    }
    let adapter = super::super::agents::adapters::by_id(&request.adapter_id)
        .context("supported agent adapter is required")?;
    let resume = (action == v1::AgentActionKind::Resume)
        .then_some(request.native_session_id.as_str())
        .filter(|id| !id.is_empty());
    if action == v1::AgentActionKind::Resume && resume.is_none() {
        bail!("resume requires a native session ID");
    }
    let launch = adapter.launch(resume);
    let agent_command = std::iter::once(launch.program.to_owned())
        .chain(launch.arguments)
        .map(|argument| shell_quote(&argument))
        .collect::<Vec<_>>()
        .join(" ");
    let shell_command = format!("exec {agent_command}");
    let placement = placement_for(request, action)?;
    {
        let baseline = topology_baseline.lock().unwrap();
        let (snapshot, baseline_identity) = baseline
            .as_ref()
            .context("authoritative topology is unavailable; refresh before launching")?;
        if baseline_identity != &identity {
            bail!("tmux server identity changed; refresh before launching an agent");
        }
        validate_placement_target(request, placement, snapshot)?;
    }
    let split = placement == v1::AgentPlacementKind::Split;
    let output = if split {
        validate_tmux_id(&request.pane_id, '%')?;
        tmux_command()?
            .args(["split-window", "-P", "-F", PLACEMENT_FORMAT])
            .arg("-t")
            .arg(&request.pane_id)
            .arg("-c")
            .arg(&request.active_root)
            .arg(shell_command)
            .output()
    } else {
        validate_tmux_id(&request.session_id, '$')?;
        tmux_command()?
            .args(["new-window", "-P", "-F", PLACEMENT_FORMAT])
            .arg("-t")
            .arg(&request.session_id)
            .arg("-c")
            .arg(&request.active_root)
            .arg(shell_command)
            .output()
    }
    .context("launch tmux agent placement")?;
    if !output.status.success() {
        bail!(
            "tmux rejected agent launch: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    let stdout =
        String::from_utf8(output.stdout).context("tmux returned non-UTF-8 placement IDs")?;
    let ids = parse_placement_ids(&stdout)?;
    // Invalidate the cached topology. The topology actor will publish the
    // authoritative post-launch snapshot and process detection result.
    *topology_baseline.lock().unwrap() = None;
    Ok(ids)
}

fn parse_placement_ids(stdout: &str) -> anyhow::Result<(String, String, String)> {
    let ids = stdout.trim_end().split(':').collect::<Vec<_>>();
    if ids.len() != 3 {
        bail!("tmux omitted launch placement identity");
    }
    validate_tmux_id(ids[0], '$')?;
    validate_tmux_id(ids[1], '@')?;
    validate_tmux_id(ids[2], '%')?;
    Ok((ids[0].into(), ids[1].into(), ids[2].into()))
}

fn placement_for(
    request: &v1::AgentRequest,
    action: v1::AgentActionKind,
) -> anyhow::Result<v1::AgentPlacementKind> {
    let placement = if action == v1::AgentActionKind::Resume {
        v1::AgentPlacementKind::try_from(request.placement).unwrap_or_default()
    } else if action == v1::AgentActionKind::LaunchSplit {
        v1::AgentPlacementKind::Split
    } else {
        v1::AgentPlacementKind::Window
    };
    if action == v1::AgentActionKind::Resume && placement == v1::AgentPlacementKind::Unspecified {
        bail!("resume requires explicit window or split placement");
    }
    Ok(placement)
}

fn validate_placement_target(
    request: &v1::AgentRequest,
    placement: v1::AgentPlacementKind,
    snapshot: &tmux_control::TmuxSnapshot,
) -> anyhow::Result<()> {
    validate_tmux_id(&request.session_id, '$')?;
    if !snapshot
        .sessions
        .iter()
        .any(|session| session.id == request.session_id)
    {
        bail!("agent placement session no longer exists");
    }
    if placement == v1::AgentPlacementKind::Split {
        validate_tmux_id(&request.window_id, '@')?;
        validate_tmux_id(&request.pane_id, '%')?;
        if !snapshot.panes.iter().any(|pane| {
            pane.id == request.pane_id
                && pane.window_id == request.window_id
                && pane.session_id == request.session_id
        }) {
            bail!("agent split placement no longer matches the exact pane identity");
        }
    }
    Ok(())
}

fn validate_tmux_id(value: &str, prefix: char) -> anyhow::Result<()> {
    if value.strip_prefix(prefix).is_some_and(|digits| {
        !digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_digit())
    }) {
        Ok(())
    } else {
        bail!("invalid tmux target ID")
    }
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_quoting_never_interpolates_resume_identifiers() {
        assert_eq!(shell_quote("abc'$(touch nope)"), "'abc'\\''$(touch nope)'");
    }

    #[test]
    fn launched_agent_executes_the_quoted_command_directly() {
        let command = format!("exec {}", shell_quote("codex"));
        assert_eq!(command, "exec 'codex'");
    }

    #[test]
    fn exact_tmux_ids_are_required_for_launch_routing() {
        assert!(validate_tmux_id("%42", '%').is_ok());
        assert!(validate_tmux_id("active", '%').is_err());
        assert!(validate_tmux_id("%2;run-shell", '%').is_err());
    }

    #[test]
    fn portable_placement_format_round_trips_exact_ids() {
        assert_eq!(PLACEMENT_FORMAT, "#{session_id}:#{window_id}:#{pane_id}");
        assert_eq!(
            parse_placement_ids("$12:@34:%56\n").unwrap(),
            ("$12".into(), "@34".into(), "%56".into())
        );
        assert!(parse_placement_ids("$12_@34_%56\n").is_err());
        assert!(parse_placement_ids("$12:@34:%56:extra\n").is_err());
    }

    #[test]
    fn resume_requires_and_honors_explicit_window_or_split_placement() {
        let mut request = v1::AgentRequest::default();
        assert!(placement_for(&request, v1::AgentActionKind::Resume).is_err());
        request.placement = v1::AgentPlacementKind::Window.into();
        assert_eq!(
            placement_for(&request, v1::AgentActionKind::Resume).unwrap(),
            v1::AgentPlacementKind::Window
        );
        request.placement = v1::AgentPlacementKind::Split.into();
        assert_eq!(
            placement_for(&request, v1::AgentActionKind::Resume).unwrap(),
            v1::AgentPlacementKind::Split
        );
    }

    #[test]
    fn split_placement_requires_the_full_exact_session_window_pane_tuple() {
        let snapshot = tmux_control::TmuxSnapshot {
            sessions: vec![tmux_control::Session {
                id: "$1".into(),
                name: "work".into(),
                window_count: 1,
                attached_clients: 0,
                order: 0,
                pinned: false,
            }],
            panes: vec![tmux_control::Pane {
                id: "%3".into(),
                session_id: "$1".into(),
                window_id: "@2".into(),
                index: 0,
                active: true,
                width: 80,
                height: 24,
                left: 0,
                top: 0,
                current_path: "/work".into(),
                current_command: "codex".into(),
                pane_pid: 0,
                start_command: String::new(),
            }],
            ..Default::default()
        };
        let mut request = v1::AgentRequest {
            session_id: "$1".into(),
            window_id: "@2".into(),
            pane_id: "%3".into(),
            ..Default::default()
        };
        assert!(
            validate_placement_target(&request, v1::AgentPlacementKind::Split, &snapshot).is_ok()
        );
        request.window_id = "@9".into();
        assert!(
            validate_placement_target(&request, v1::AgentPlacementKind::Split, &snapshot).is_err()
        );
    }
}
