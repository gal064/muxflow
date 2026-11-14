//! What the session history learns from this service: every agent with a
//! native session, after each state change and each topology reconcile. The
//! store itself is `service::history`.

use std::{
    collections::BTreeMap,
    sync::{Mutex, PoisonError},
};

use super::StoredState;
use crate::service::history::{History, Seen, tab_title};

/// Each pane's folder and whether it is a chat, from the last topology
/// reconciled: a session id often arrives by hook after that reconcile, and
/// its folder is wanted then, not at the next one.
static PANES: Mutex<BTreeMap<String, (String, bool)>> = Mutex::new(BTreeMap::new());

/// Tells the history which sessions run now, under which title, workspace and
/// folder. `topology` is the snapshot just reconciled, when there is one: it
/// is what knows each pane's folder and whether it is a chat.
pub(super) fn observe(state: &StoredState, topology: Option<&tmux_control::TmuxSnapshot>) {
    let mut panes = PANES.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some(topology) = topology {
        *panes = topology
            .panes
            .iter()
            .map(|pane| {
                let folder = (pane.current_path.clone(), !pane.chat_adapter_id.is_empty());
                (pane.id.clone(), folder)
            })
            .collect();
    }
    let seen = seen(state, topology, &panes);
    drop(panes);
    History::global().observe(seen);
}

fn seen(
    state: &StoredState,
    topology: Option<&tmux_control::TmuxSnapshot>,
    panes: &BTreeMap<String, (String, bool)>,
) -> Vec<Seen> {
    state
        .agents
        .values()
        // Only an agent in a pane runs a session. A record a late hook made for
        // a pane already gone (one a snooze just closed) does not.
        .filter(|record| {
            !record.native_session_id.is_empty()
                && !record.adapter_id.is_empty()
                && !record.route.pane_id.is_empty()
                && topology.is_none_or(|topology| {
                    topology
                        .panes
                        .iter()
                        .any(|pane| pane.id == record.route.pane_id)
                })
        })
        .map(|record| {
            let route = &record.route;
            let pane = panes.get(&route.pane_id);
            let last_active = record
                .lifecycle_observed_at_unix_millis
                .max(record.lifecycle_changed_at_unix_millis);
            Seen {
                adapter_id: record.adapter_id.clone(),
                native_session_id: record.native_session_id.clone(),
                agent_id: record.agent_id.clone(),
                pane_id: route.pane_id.clone(),
                title: tab_title(&route.pane_title_fallback, &route.window_name_fallback),
                fallback_title: super::adapters::by_id(&record.adapter_id).map_or_else(
                    || record.display_name.clone(),
                    |adapter| adapter.display_name().to_owned(),
                ),
                workspace_name: route.session_name_fallback.clone(),
                cwd: pane.map(|(folder, _)| folder.clone()),
                chat: pane.map(|(_, chat)| *chat),
                last_active_at: if last_active > 0 {
                    last_active
                } else {
                    record.updated_at_unix_millis
                },
            }
        })
        .collect()
}
