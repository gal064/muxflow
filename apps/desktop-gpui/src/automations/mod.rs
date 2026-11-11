//! Automations: a host's scheduled agent runs (docs/automations.md), as the
//! right panel's "automations" segment. Port of `features/automations`.
//!
//! The panel is host-scoped like Ports: it lists the host its chooser names,
//! through that host's live bridge, and lists again when the host says its
//! automations changed.

pub mod model;
pub mod panel;
pub mod view;

#[cfg(test)]
mod tests;

use std::{rc::Rc, sync::Arc};

use desktop_core::app_state::load_app_state;
use gpui_kit::{App, AppContext, Entity, Global, IntoElement, base::Root};

use crate::{
    app::Core,
    connection_ui::{
        backend::TerminalBackend,
        connection_reducer::ConnectionPhase,
        controller::{ConnectionController, ConnectionEvent},
    },
    ui::chrome::PanelBodies,
};

use panel::{AutomationsPanel, BackendTransport, PanelEvent, PanelHost, system_clock};

pub struct Automations {
    pub panel: Entity<AutomationsPanel>,
}

impl Global for Automations {}

/// Called once from [`crate::features::init`].
pub fn init(cx: &mut App) {
    Root::register_plugin::<view::AutomationsSurface>(cx, view::AutomationsSurface::new);
    PanelBodies::register(
        "automations",
        |_, cx| match cx.try_global::<Automations>() {
            Some(automations) => automations.panel.clone().into_any_element(),
            None => gpui_kit::div().into_any_element(),
        },
        cx,
    );
}

/// Called once the connection controller exists, before the first window.
pub fn start(
    controller: Entity<ConnectionController>,
    backend: Arc<dyn TerminalBackend>,
    cx: &mut App,
) {
    let panel =
        cx.new(|_| AutomationsPanel::new(Rc::new(BackendTransport(backend)), system_clock()));
    cx.set_global(Automations {
        panel: panel.clone(),
    });
    cx.subscribe(&controller, {
        let panel = panel.clone();
        move |_, event, cx| {
            if let ConnectionEvent::AutomationChanged { profile_id } = event {
                panel.update(cx, |panel, cx| panel.automation_changed(profile_id, cx));
            }
        }
    })
    .detach();
    cx.observe(&controller, {
        let panel = panel.clone();
        move |controller, cx| sync(&controller, &panel, cx)
    })
    .detach();
    cx.subscribe(&panel, {
        let controller = controller.clone();
        move |_, event, cx| match event {
            PanelEvent::Message(message) => {
                controller.update(cx, |controller, cx| {
                    controller.report_status(message.clone(), cx)
                });
            }
            PanelEvent::OpenPane {
                profile_id,
                pane_id,
                source,
            } => {
                if crate::agents::open_pane(profile_id, pane_id, source, cx).is_err() {
                    controller.update(cx, |controller, cx| {
                        controller.report_status(format!("{source}'s tab is no longer open."), cx)
                    });
                }
            }
        }
    })
    .detach();
    sync(&controller, &panel, cx);
}

/// The shown hosts, as the panel needs them.
fn sync(controller: &Entity<ConnectionController>, panel: &Entity<AutomationsPanel>, cx: &mut App) {
    let defaults = load_app_state(&cx.global::<Core>().app_state)
        .map(|state| state.workspace_defaults)
        .unwrap_or_default();
    let controller = controller.read(cx);
    let hosts = controller
        .links()
        .map(|link| {
            let state = &link.host_state;
            PanelHost {
                profile_id: link.profile_id.clone(),
                label: controller
                    .profiles()
                    .iter()
                    .find(|profile| profile.id == link.profile_id)
                    .map_or_else(|| link.profile_id.clone(), |profile| profile.label.clone()),
                client_id: link
                    .client_id
                    .clone()
                    .filter(|_| state.phase == ConnectionPhase::Connected),
                server_identity: state.server_identity.clone(),
                panes: state
                    .snapshot
                    .panes
                    .iter()
                    .map(|pane| pane.id.clone())
                    .collect(),
                start_directory: defaults
                    .get(&link.profile_id)
                    .and_then(|defaults| defaults.directory.clone()),
            }
        })
        .collect();
    let active = controller.active_profile_id().to_owned();
    panel.update(cx, |panel, cx| panel.set_hosts(hosts, active, cx));
}
