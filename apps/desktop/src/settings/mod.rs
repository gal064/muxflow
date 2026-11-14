//! Settings: every preference the user can change, live, and the dialog that
//! edits them. Port of `features/shell/SettingsDialog.tsx`; the screen-reader
//! terminal mirror setting is an agreed exception and has no counterpart.
//!
//! # For features that read a setting
//!
//! Read [`get`] when the value is needed, not at start: a change in the dialog
//! lands in the global before the dialog paints, so no reader has to reread the
//! saved file. A reader that has to redraw on a change observes the global with
//! `cx.observe_global::<Settings>(...)`.
//!
//! # For features Settings renders something of
//!
//! Two controls belong to a feature that is not a preference:
//! [`set_agent_setup`] publishes "Set up agent status…" and [`set_notifier`]
//! the native notifier the Sounds tab asks about the permission.
//!
//! Saves go through `update_app_state`, which holds the store's write lock
//! across the read and the write, so they cannot overwrite the shell layout or
//! the window geometry another module saved in between.

pub mod dialog;
pub mod prefs;

use std::{
    future::Future,
    rc::Rc,
    sync::{Arc, LazyLock, Mutex},
    time::Duration,
};

use desktop_core::{
    app_state::{PersistedAppState, load_app_state, update_app_state},
    notifications::NativeNotifications,
    platform,
};
use gpui_kit::{App, BackgroundExecutor, Global, Window};
pub use prefs::Prefs;
use tokio::task::JoinHandle;

use crate::{app::Core, notices, theme};

/// The live preferences. Written only through [`update`].
pub struct Settings {
    prefs: Prefs,
    /// What the save writes. A save task reads it when it runs, so two saves
    /// that run out of order still end on the newest values.
    latest: Arc<Mutex<Prefs>>,
}

impl Global for Settings {}

/// The defaults, for an app with no settings yet: a test window that did not
/// start the features.
static DEFAULTS: LazyLock<Prefs> =
    LazyLock::new(|| Prefs::from_state(&PersistedAppState::default()));

/// The preferences as they are now.
pub fn get(cx: &App) -> &Prefs {
    cx.try_global::<Settings>()
        .map_or(&DEFAULTS, |settings| &settings.prefs)
}

/// How often a pending save or check is looked at. Polled, not awaited:
/// GPUI's test scheduler rejects a wake from a runtime thread.
const POLL: Duration = Duration::from_millis(50);

/// Waits for work on the backend's runtime without a runtime thread waking a
/// GPUI task.
pub(crate) async fn finished<T>(
    handle: JoinHandle<T>,
    executor: &BackgroundExecutor,
) -> Result<T, String> {
    while !handle.is_finished() {
        executor.timer(POLL).await;
    }
    handle.await.map_err(|error| error.to_string())
}

/// Runs `future` on the backend's runtime and waits for it on a GPUI timer.
pub(crate) fn on_runtime<T: Send + 'static>(
    future: impl Future<Output = T> + Send + 'static,
    executor: BackgroundExecutor,
) -> impl Future<Output = Result<T, String>> {
    let handle = platform::spawn(future);
    async move { finished(handle, &executor).await }
}

/// Changes preferences: the new values are live at once, and the save runs in
/// the background. A save that fails is said, and the values stay changed for
/// this run.
pub fn update(change: impl FnOnce(&mut Prefs), window: &mut Window, cx: &mut App) {
    let before = get(cx).clone();
    let mut after = before.clone();
    change(&mut after);
    if after == before {
        return;
    }
    let settings = cx.global_mut::<Settings>();
    settings.prefs = after.clone();
    *settings.latest.lock().unwrap() = after.clone();
    if (after.theme, after.ui_scale) != (before.theme, before.ui_scale) {
        let system = cx.window_appearance();
        theme::apply(after.theme, after.ui_scale, system, cx);
        cx.refresh_windows();
    }
    let store = cx.global::<Core>().app_state.clone();
    // Unreadable saved state takes no writes until it is reset, and the
    // shell already says so once: the change lives for this run.
    if load_app_state(&store).is_err() {
        return;
    }
    let latest = cx.global::<Settings>().latest.clone();
    let save = on_runtime(
        async move {
            update_app_state(&store, move |state| latest.lock().unwrap().apply_to(state)).await
        },
        cx.background_executor().clone(),
    );
    window
        .spawn(cx, async move |cx| {
            let failure = match save.await {
                Ok(Ok(())) => return,
                Ok(Err(failure)) | Err(failure) => failure,
            };
            cx.update(|window, cx| {
                notices::notify(&format!("Could not save settings: {failure}"), window, cx);
            })
            .ok();
        })
        .detach();
}

/// What the Connection tab says about "Set up agent status…": whether this
/// host can be set up, already reports, or has not connected.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AgentSetupStatus {
    pub available: bool,
    pub reports: bool,
    pub connected: bool,
}

/// Offers the agent setup in a window.
pub type SetUp = Rc<dyn Fn(&mut Window, &mut App)>;

#[derive(Clone)]
pub struct AgentSetup {
    pub status: Rc<dyn Fn(&App) -> AgentSetupStatus>,
    /// Closes Settings first (the dialog does) and then offers the setup.
    pub set_up: SetUp,
}

struct AgentSetupSlot(AgentSetup);

impl Global for AgentSetupSlot {}

/// Publishes the agent setup flow Settings offers. Until a feature does, the
/// button is off and says a host has to connect.
pub fn set_agent_setup(setup: AgentSetup, cx: &mut App) {
    cx.set_global(AgentSetupSlot(setup));
}

pub(crate) fn agent_setup(cx: &App) -> Option<AgentSetup> {
    cx.try_global::<AgentSetupSlot>().map(|slot| slot.0.clone())
}

struct Notifier(NativeNotifications);

impl Global for Notifier {}

/// Publishes the app's native notifier. It is created once, with the real
/// activation sink, by whichever feature routes notification clicks, so Settings
/// asks that one instead of making a second.
pub fn set_notifier(notifier: NativeNotifications, cx: &mut App) {
    cx.set_global(Notifier(notifier));
}

fn notifier(cx: &App) -> Option<NativeNotifications> {
    cx.try_global::<Notifier>().map(|slot| slot.0.clone())
}

/// Makes `state`'s preferences the live ones, as after saved shell state was
/// reset, without saving them again.
pub fn reload(state: &PersistedAppState, cx: &mut App) {
    if !cx.has_global::<Settings>() {
        return;
    }
    let prefs = Prefs::from_state(state);
    let settings = cx.global_mut::<Settings>();
    *settings.latest.lock().unwrap() = prefs.clone();
    settings.prefs = prefs.clone();
    let system = cx.window_appearance();
    theme::apply(prefs.theme, prefs.ui_scale, system, cx);
    cx.refresh_windows();
}

/// Called once from [`crate::features::init`].
pub fn init(cx: &mut App) {
    let state = load_app_state(&cx.global::<Core>().app_state).unwrap_or_default();
    let prefs = Prefs::from_state(&state);
    cx.set_global(Settings {
        latest: Arc::new(Mutex::new(prefs.clone())),
        prefs,
    });
    dialog::init(cx);
}
