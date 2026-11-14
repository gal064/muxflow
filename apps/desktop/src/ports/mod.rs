//! Ports: forwarding a host's port to this machine, as the right panel's third
//! segment. Port of `features/ports`.
//!
//! The panel lists the shown SSH hosts through the connection controller, and
//! asks `desktop-core` for forwards directly. Forwards live as long as this
//! process: `desktop-core` ties each to a pipe this process holds.

pub mod model;
pub mod panel;
mod view;

#[cfg(test)]
mod tests;

use std::{cell::Cell, rc::Rc};

use desktop_core::connection::ConnectionSpec;
use gpui_kit::{AnyElement, App, AppContext, Entity, IntoElement, Window, div};

use crate::{connection_ui::controller::ConnectionController, ui::chrome::PanelBodies};

use model::PortsHost;
use panel::{NativePorts, Ports, PortsPanel};

/// Called once from [`crate::features::init`].
pub fn init(cx: &mut App) {
    PanelBodies::register("ports", body, cx);
}

/// The segment's body. The panel opens on a fresh detection each time the
/// segment is shown, as a body mounted on selection did: the keyed state below
/// lives exactly as long as the body is drawn every frame.
fn body(window: &mut Window, cx: &mut App) -> AnyElement {
    let Some(panel) = cx.try_global::<Ports>().map(|ports| ports.0.clone()) else {
        return div().into_any_element();
    };
    let mounted = Rc::new(Cell::new(false));
    let seen = mounted.clone();
    window.use_keyed_state("ports-mounted", cx, move |_, _| seen.set(true));
    if mounted.get() {
        panel.update(cx, |panel, cx| panel.detect(cx));
    }
    panel.into_any_element()
}

/// Called once the connection controller exists, before the first window.
pub fn start(controller: Entity<ConnectionController>, cx: &mut App) {
    let native = Rc::new(NativePorts::default());
    let panel = cx.new(|cx| {
        let mut panel = PortsPanel::new(native.clone());
        panel.start_polling(cx);
        panel
    });
    cx.set_global(Ports(panel.clone()));
    cx.observe(&controller, {
        let panel = panel.clone();
        move |controller, cx| sync(&controller, &panel, cx)
    })
    .detach();
    sync(&controller, &panel, cx);
    // A graceful quit ends the forwards itself rather than rely on the pipe.
    cx.on_app_quit(move |_| {
        native.stop_all();
        async {}
    })
    .detach();
}

/// The shown SSH hosts, as the panel needs them. Local has nothing to forward.
fn sync(controller: &Entity<ConnectionController>, panel: &Entity<PortsPanel>, cx: &mut App) {
    let controller = controller.read(cx);
    let hosts: Vec<PortsHost> = controller
        .links()
        .filter(|link| matches!(link.connection, ConnectionSpec::Ssh { .. }))
        .map(|link| PortsHost {
            profile_id: link.profile_id.clone(),
            label: controller
                .profiles()
                .iter()
                .find(|profile| profile.id == link.profile_id)
                .map_or_else(|| link.profile_id.clone(), |profile| profile.label.clone()),
            connection: link.connection.clone(),
        })
        .collect();
    let active = controller.active_profile_id();
    panel.update(cx, |panel, cx| {
        if panel.hosts() != hosts || panel.active_profile() != active {
            panel.set_hosts(hosts, active, cx);
        }
    });
}
