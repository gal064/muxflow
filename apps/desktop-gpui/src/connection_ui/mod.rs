//! Owned by T2.4 `connections`: host profiles, connection, setup and
//! missing-helper recovery, driving `desktop-core`. Everything goes under
//! `connection_ui/`; the app joins through [`init`] and, once running for
//! real, [`start`].
//!
//! The state machines are plain functions over plain values, ported with
//! their tests. [`controller::ConnectionController`] holds them and owns every
//! bridge, behind [`backend::ConnectionBackend`]. [`surface`] and [`dialog`]
//! are the only modules that know about windows.

pub mod app_recovery;
pub mod backend;
pub mod bridge_events;
pub mod connection_reducer;
pub mod controller;
pub mod dialog;
pub mod helper_recovery;
pub mod helper_upgrade;
pub mod host_links;
pub mod host_profiles;
pub mod host_scope;
pub mod link_quality;
pub mod resume_recovery;
pub mod status_notice;
pub mod surface;

use std::sync::Arc;

use desktop_core::{
    connection::{self, ProfileStore},
    incidents::IncidentJournal,
    platform::{AppPaths, EventBody, EventSink},
    power_events,
};
use futures::{StreamExt, channel::mpsc};
use gpui_kit::{App, AppContext, Entity, Global, base::Root};

use crate::app::Core;

use backend::NativeBackend;
use controller::ConnectionController;
use surface::ConnectionSurface;

/// The running controller. Absent until [`start`], so a test window never
/// connects to anything.
pub struct ConnectionUi(pub Entity<ConnectionController>);

impl Global for ConnectionUi {}

/// Called once from [`crate::features::init`].
pub fn init(cx: &mut App) {
    dialog::init(cx);
    Root::register_plugin::<ConnectionSurface>(cx, ConnectionSurface::new);
}

struct ResumeSink(mpsc::UnboundedSender<()>);

impl EventSink for ResumeSink {
    fn send(&self, _: EventBody) -> Result<(), String> {
        self.0
            .unbounded_send(())
            .map_err(|_| "the connection UI stopped listening".to_owned())
    }
}

/// Loads the saved hosts and connects, as the Tauri app's setup does. Called
/// from [`crate::app::run`], after the windowless startup, before the first
/// window opens.
pub fn start(cx: &mut App) -> Result<(), String> {
    let core = cx.global::<Core>();
    let config_dir = core.paths.app_config_dir()?;
    let log_dir = core.paths.app_log_dir()?;
    let app_state = core.app_state.clone();
    let profiles = Arc::new(ProfileStore::load(config_dir.join("profiles.json"))?);
    connection::spawn_orphan_reaper();
    let backend = Arc::new(NativeBackend::new(
        app_state.clone(),
        IncidentJournal::new(log_dir),
    ));
    let controller = cx.new(|cx| ConnectionController::new(backend, profiles, app_state, cx));
    cx.set_global(ConnectionUi(controller.clone()));

    let (sender, mut resumed) = mpsc::unbounded();
    power_events::start(Arc::new(ResumeSink(sender)));
    cx.spawn(async move |cx| {
        while resumed.next().await.is_some() {
            controller.update(cx, |controller, cx| controller.desktop_resumed(cx));
        }
    })
    .detach();

    cx.on_app_quit(|_| async { connection::close_all_control_masters() })
        .detach();
    Ok(())
}
