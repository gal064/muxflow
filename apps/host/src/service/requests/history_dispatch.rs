//! `OPERATION_SESSION_HISTORY` (docs/design/chat-plan.md §4 and §5 E).
//!
//! The record of sessions is `service::history`; this maps a request onto it,
//! and does the tmux half of the two actions that have one: a resume opens a
//! tab, in the workspace named like the one the session ran in or in a new one
//! made in its folder, and a snooze closes the running agent's pane.

use std::sync::{
    Mutex,
    atomic::{AtomicU64, Ordering},
};

use anyhow::{Context, bail};
use tmux_agent_protocol::v1;

use super::super::{
    agents::{AgentRuntime, adapters},
    history::{self, History, Refusal, refusal},
    snapshot::{server_identity, tmux_command},
    terminal::validate_tmux_id,
    tmux_actions::{
        configure_new_session, escaped_format_literal, resolved_start_directory, run_for_ids,
        validate_name,
    },
};
use super::agent_dispatch::{PLACEMENT_FORMAT, agent_launch_argv, parse_placement_ids};
use super::response_error;

/// A snooze further out than this is a mistake, not a plan.
const MAX_SNOOZE_MILLIS: i64 = 366 * 24 * 60 * 60 * 1_000;

type Baseline = Mutex<Option<(tmux_control::TmuxSnapshot, String)>>;

pub(super) fn handle(
    request: Option<v1::HistoryRequest>,
    generation: &AtomicU64,
    topology_baseline: &Baseline,
) -> v1::Response {
    match request.map(|request| dispatch(request, generation, topology_baseline)) {
        None => response_error(
            "history_invalid_request",
            "A history operation needs a history payload and an action.",
        ),
        Some(Ok(history)) => v1::Response {
            ok: true,
            history: Some(history),
            ..Default::default()
        },
        Some(Err(error)) => response_error(error.code, &error.message),
    }
}

fn dispatch(
    request: v1::HistoryRequest,
    generation: &AtomicU64,
    topology_baseline: &Baseline,
) -> Result<v1::HistoryResponse, Refusal> {
    use v1::HistoryAction as Action;
    let store = History::global();
    let action = Action::try_from(request.action).unwrap_or_default();
    if action == Action::List {
        return Ok(store.list());
    }
    if action == Action::Unspecified {
        return Err(invalid("Unknown history action."));
    }
    if adapters::by_id(&request.adapter_id).is_none()
        || request.native_session_id.is_empty()
        || request.native_session_id.len() > history::MAX_SESSION_ID_BYTES
    {
        return Err(invalid("A history action names a recorded session."));
    }
    let entry = store
        .lookup(&request.adapter_id, &request.native_session_id)
        .ok_or_else(|| refusal("history_unknown_session", "This session is not in History."))?;
    let (event, placed) = match action {
        Action::Resume => {
            let placed = resume(&request, &entry, generation, topology_baseline)?;
            // The tab is open whether or not its placeholder could be cleared.
            (
                store
                    .resumed(&entry.adapter_id, &entry.native_session_id)
                    .ok(),
                placed,
            )
        }
        Action::Snooze => (
            Some(snooze(&request, &entry, generation, topology_baseline)?),
            Default::default(),
        ),
        Action::Wake => (
            Some(store.wake(&entry.adapter_id, &entry.native_session_id)?),
            Default::default(),
        ),
        Action::List | Action::Unspecified => unreachable!("answered above"),
    };
    let generation = event.as_ref().map_or(0, |event| event.generation);
    if let Some(event) = event {
        history::publish(event);
    }
    let (session_id, window_id, pane_id) = placed;
    Ok(v1::HistoryResponse {
        generation,
        session_id,
        window_id,
        pane_id,
        ..Default::default()
    })
}

fn invalid(message: &str) -> Refusal {
    refusal("history_invalid_request", message)
}

/// The caller acts on the tmux server and topology it last saw, as an agent
/// launch does.
fn check_scope(request: &v1::HistoryRequest, generation: &AtomicU64) -> Result<String, Refusal> {
    let identity = server_identity();
    if request.expected_server_identity != identity {
        return Err(refusal(
            "history_server_changed",
            "The tmux server changed; refresh and try again.",
        ));
    }
    if request.expected_topology_generation != generation.load(Ordering::Acquire) {
        return Err(refusal(
            "history_topology_changed",
            "Workspaces changed while this was asked; try again.",
        ));
    }
    Ok(identity)
}

/// Opens the session in a new tab. Answers its session, window and pane.
fn resume(
    request: &v1::HistoryRequest,
    entry: &v1::HistoryEntry,
    generation: &AtomicU64,
    topology_baseline: &Baseline,
) -> Result<(String, String, String), Refusal> {
    if entry.running {
        return Err(refusal(
            "history_running",
            "This session is already running; open its tab instead.",
        ));
    }
    let identity = check_scope(request, generation)?;
    let store = History::global();
    if !store.claim_resume(
        &entry.adapter_id,
        &entry.native_session_id,
        history::now_millis(),
    ) {
        return Err(refusal(
            "history_running",
            "This session is already opening.",
        ));
    }
    let placed = place_in_workspace(entry, &identity, topology_baseline);
    if placed.is_err() {
        store.release_resume(&entry.adapter_id, &entry.native_session_id);
    }
    placed
}

fn place_in_workspace(
    entry: &v1::HistoryEntry,
    identity: &str,
    topology_baseline: &Baseline,
) -> Result<(String, String, String), Refusal> {
    let workspace = {
        let baseline = topology_baseline.lock().unwrap();
        let (snapshot, baseline_identity) = baseline.as_ref().ok_or_else(|| {
            refusal(
                "history_topology_changed",
                "Workspaces are still loading; try again.",
            )
        })?;
        if baseline_identity != identity {
            return Err(refusal(
                "history_server_changed",
                "The tmux server changed; refresh and try again.",
            ));
        }
        snapshot
            .sessions
            .iter()
            .find(|session| {
                !entry.workspace_name.is_empty() && session.name == entry.workspace_name
            })
            .map(|session| session.id.clone())
    };
    let placed = place(entry, workspace).map_err(|error| {
        refusal(
            "history_resume_failed",
            format!("The session could not be resumed: {error:#}"),
        )
    })?;
    // The topology actor publishes the snapshot with the new tab.
    *topology_baseline.lock().unwrap() = None;
    Ok(placed)
}

fn place(
    entry: &v1::HistoryEntry,
    workspace: Option<String>,
) -> anyhow::Result<(String, String, String)> {
    let adapter = adapters::by_id(&entry.adapter_id).context("unknown agent adapter")?;
    // A folder that is gone is refused rather than left to tmux, which would
    // start the agent somewhere its conversation knows nothing about.
    let folder = if entry.cwd.is_empty() {
        None
    } else {
        Some(resolved_start_directory(&entry.cwd)?)
    };
    let session_id = match workspace {
        Some(session_id) => session_id,
        None => {
            let folder = folder
                .as_deref()
                .context("its folder was never seen, so there is nowhere to make its workspace")?;
            let name = if entry.workspace_name.is_empty() {
                adapter.display_name().to_owned()
            } else {
                entry.workspace_name.clone()
            };
            validate_name(&name)?;
            let mut command = tmux_command()?;
            configure_new_session(
                &mut command,
                &v1::TmuxAction {
                    name,
                    directory: folder.to_owned(),
                    ..Default::default()
                },
                false,
            )?;
            run_for_ids(command, &['$', '@', '%'])?.swap_remove(0)
        }
    };
    validate_tmux_id(&session_id, '$')?;
    let mut command = tmux_command()?;
    command
        .args(["new-window", "-P", "-F", PLACEMENT_FORMAT])
        .arg("-t")
        .arg(&session_id);
    if let Some(folder) = &folder {
        command.arg("-c").arg(escaped_format_literal(folder));
    }
    let output = command
        .args(agent_launch_argv(
            adapter.launch(Some(&entry.native_session_id)),
        ))
        .output()
        .context("open the resumed session's tab")?;
    if !output.status.success() {
        bail!(
            "tmux refused the tab: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    parse_placement_ids(&String::from_utf8(output.stdout).context("tmux answered non-UTF-8")?)
}

/// Closes the running agent's pane, if it runs, and records the wake time.
fn snooze(
    request: &v1::HistoryRequest,
    entry: &v1::HistoryEntry,
    generation: &AtomicU64,
    topology_baseline: &Baseline,
) -> Result<v1::HistoryEvent, Refusal> {
    let now = history::now_millis();
    let until = request.snooze_until_unix_millis;
    if until <= now || until - now > MAX_SNOOZE_MILLIS {
        return Err(invalid("A snooze wakes at a time in the coming year."));
    }
    let pane = if entry.running {
        let identity = check_scope(request, generation)?;
        Some(running_pane(entry, &identity)?)
    } else {
        None
    };
    // Recorded first: a session closed with no wake time on record would just
    // be gone. A pane that will not close puts back what was there before.
    let store = History::global();
    let event = store.snooze(&entry.adapter_id, &entry.native_session_id, until)?;
    if let Some(pane) = &pane {
        if let Err(error) = close_pane(pane) {
            let _ = store.restore(
                &entry.adapter_id,
                &entry.native_session_id,
                entry.snoozed_until_unix_millis,
                entry.woke,
            );
            return Err(refusal(
                "history_snooze_failed",
                format!("The session could not be closed: {error:#}"),
            ));
        }
        *topology_baseline.lock().unwrap() = None;
    }
    Ok(event)
}

/// The pane of the agent running the session, unless it is busy.
fn running_pane(entry: &v1::HistoryEntry, identity: &str) -> Result<String, Refusal> {
    let snapshot = AgentRuntime::global().snapshot();
    let record = snapshot
        .agents
        .iter()
        .find(|record| record.agent_id == entry.agent_id)
        .ok_or_else(|| {
            refusal(
                "history_snooze_failed",
                "This session runs on another tmux server.",
            )
        })?;
    let lifecycle = v1::AgentLifecycleState::try_from(record.lifecycle).unwrap_or_default();
    if matches!(
        lifecycle,
        v1::AgentLifecycleState::Working | v1::AgentLifecycleState::Blocked
    ) {
        return Err(refusal(
            "history_agent_busy",
            "An agent that is working or waiting for an answer cannot be snoozed.",
        ));
    }
    let route = record.route.as_ref();
    match route.map(|route| (route.server_identity.as_str(), route.pane_id.as_str())) {
        Some((server, pane)) if server == identity && validate_tmux_id(pane, '%').is_ok() => {
            Ok(pane.to_owned())
        }
        _ => Err(refusal(
            "history_snooze_failed",
            "This session's pane is not known, so it cannot be closed.",
        )),
    }
}

/// A pane that is already gone counts as closed: the session is not running,
/// which is all a snooze asks.
fn close_pane(pane_id: &str) -> anyhow::Result<()> {
    let output = tmux_command()?
        .args(["kill-pane", "-t", pane_id])
        .output()
        .context("close the snoozed session's pane")?;
    let stderr = String::from_utf8_lossy(&output.stderr);
    if !output.status.success() && !stderr.contains("can't find pane") {
        bail!("{}", stderr.trim());
    }
    Ok(())
}
