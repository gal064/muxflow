//! Host-side workspace restore: the Workspace tab's switch that asks the
//! host on screen to remember its tmux sessions — with the directory of
//! every window — and recreate them when its server dies. The host owns the
//! record (its `workspace_restore` plugin); this only reads and moves the
//! switch, so the phone and every desktop see the same answer.
//!
//! The switch is the host's, not a preference: nothing is saved locally, and
//! each host is asked what it holds when its link connects.

use std::{collections::BTreeMap, sync::Arc};

use desktop_core::connection::workspace_restore::WorkspaceRestoreState;
use gpui_kit::{App, Entity, Global};

use crate::connection_ui::{
    backend::TerminalBackend,
    connection_reducer::ConnectionPhase,
    controller::{ConnectionController, ConnectionEvent},
};

/// One host's switch as it was last answered.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HostRestore {
    /// Whether the host remembers and recreates. `None` until a connected
    /// host has answered.
    pub enabled: Option<bool>,
    /// What the host's record holds, in words, for the dialog's second hint.
    /// `None` when there is nothing worth saying.
    pub sessions_note: Option<String>,
    /// A change is on its way to the host.
    pub changing: bool,
}

impl Global for HostRestore {}

/// The feature: who to ask, and what each host last said. The [`HostRestore`]
/// global mirrors the host on screen for the Settings dialog to observe.
#[derive(Clone)]
struct Restore {
    controller: Entity<ConnectionController>,
    backend: Arc<dyn TerminalBackend>,
    hosts: BTreeMap<String, HostRestore>,
}

impl Global for Restore {}

/// Called once the connection controller exists, before the first window.
pub fn start(
    controller: Entity<ConnectionController>,
    backend: Arc<dyn TerminalBackend>,
    cx: &mut App,
) {
    cx.set_global(Restore {
        controller: controller.clone(),
        backend,
        hosts: BTreeMap::new(),
    });
    // A link that connects — first time or after a resync — is a host worth
    // asking, because the helper may have restarted in between.
    cx.subscribe(&controller, move |_, event, cx| {
        if let ConnectionEvent::LinkRestored { profile_id } = event {
            refresh(profile_id, cx);
        }
    })
    .detach();
    // The dialog shows the host on screen, so a host switch republishes.
    cx.observe(&controller, |_, cx| publish(cx)).detach();
    publish(cx);
}

/// Asks one connected host what it holds. Errors quiet into the status
/// channel: a host that does not know the operation is simply not asked
/// again until its link reconnects.
fn refresh(profile_id: &str, cx: &mut App) {
    let Some(ask) = connected_request(profile_id, None, cx) else {
        return;
    };
    let profile_id = profile_id.to_owned();
    cx.spawn(async move |cx| {
        let result = ask.answer.await;
        cx.update(|cx| apply(&profile_id, result, cx));
    })
    .detach();
}

/// Moves the on-screen host's switch. The Workspace tab calls it.
pub fn set_enabled(enabled: bool, cx: &mut App) {
    let profile_id = cx
        .try_global::<Restore>()
        .map(|feature| feature.controller.read(cx).active_profile_id())
        .unwrap_or_default();
    if cx
        .global::<Restore>()
        .hosts
        .get(&profile_id)
        .is_some_and(|host| host.changing)
    {
        return;
    }
    let Some(ask) = connected_request(&profile_id, Some(enabled), cx) else {
        return;
    };
    let mut feature = cx.global::<Restore>().clone();
    feature
        .hosts
        .entry(profile_id.clone())
        .or_default()
        .changing = true;
    cx.set_global(feature);
    publish(cx);
    cx.spawn(async move |cx| {
        let result = ask.answer.await;
        cx.update(|cx| apply(&profile_id, result, cx));
    })
    .detach();
}

/// A request to the named host, if a live bridge stands behind it.
struct Ask {
    answer: futures::future::BoxFuture<'static, Result<WorkspaceRestoreState, String>>,
}

fn connected_request(profile_id: &str, set_enabled: Option<bool>, cx: &App) -> Option<Ask> {
    let feature = cx.try_global::<Restore>()?;
    let controller = feature.controller.clone();
    let backend = feature.backend.clone();
    let link = controller.read(cx).link(profile_id)?;
    let client_id = link
        .client_id
        .clone()
        .filter(|_| link.host_state.phase == ConnectionPhase::Connected)?;
    Some(Ask {
        answer: backend.workspace_restore_request(client_id, set_enabled),
    })
}

/// Records the host's answer, or says in the status channel what it refused,
/// and republishes for the dialog.
fn apply(profile_id: &str, result: Result<WorkspaceRestoreState, String>, cx: &mut App) {
    let mut feature = match cx.try_global::<Restore>() {
        Some(feature) => feature.clone(),
        None => return,
    };
    let refusal = match &result {
        Ok(_) => None,
        Err(error) => Some(format!("Workspace restore: {error}")),
    };
    match result {
        // The host's answer replaces everything we thought we knew.
        Ok(state) => {
            let note = match state.enabled {
                false => None,
                true if state.sessions.is_empty() => Some(
                    "Nothing remembered yet: the host writes down what is open within a quarter-minute of the switch going on."
                        .to_owned(),
                ),
                true => {
                    let mut names: Vec<&str> =
                        state.sessions.iter().take(6).map(|s| s.name.as_str()).collect();
                    let more = state.sessions.len() - names.len();
                    if more > 0 {
                        names.push("…");
                    }
                    Some(format!("Remembered, ready for the next server: {}.", names.join(", ")))
                }
            };
            feature.hosts.insert(
                profile_id.to_owned(),
                HostRestore {
                    enabled: Some(state.enabled),
                    sessions_note: note,
                    changing: false,
                },
            );
        }
        // A refused change leaves the switch where the host last said it
        // was: the host keeps this setting, not us.
        Err(_) => {
            feature
                .hosts
                .entry(profile_id.to_owned())
                .or_default()
                .changing = false
        }
    }
    let controller = feature.controller.clone();
    cx.set_global(feature);
    publish(cx);
    if let Some(message) = refusal {
        controller.update(cx, |controller, cx| controller.report_status(message, cx));
    }
}

/// Publishes the host on screen for the Settings dialog to read.
fn publish(cx: &mut App) {
    let Some(feature) = cx.try_global::<Restore>() else {
        return;
    };
    let active = feature.controller.read(cx).active_profile_id();
    let restore = feature.hosts.get(&active).cloned().unwrap_or_default();
    cx.set_global(restore);
}
