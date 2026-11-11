//! The single place a feature joins the app.
//!
//! Each feature module exposes `pub fn init(cx: &mut App)`, called here before
//! the first window opens. Register global state, actions and key bindings from
//! there. For per-window setup, use `cx.observe_new::<Shell>(...)` from `init`,
//! which runs for every window as its [`Shell`] is created. Reach the backend
//! through [`Core`], and put presentation in the feature's own module.
//!
//! A new feature adds one line below and touches nothing else in the app root.
//!
//! [`Shell`]: crate::ui::shell::Shell
//! [`Core`]: crate::app::Core

use gpui_kit::App;

pub fn init(cx: &mut App) {
    crate::commands::init(cx);
    // Before the features that read a preference as they start, and before the
    // menus, which show Settings once it has a handler.
    crate::settings::init(cx);
    crate::menus::init(cx);
    crate::connection_ui::init(cx);
    crate::workspaces::init(cx);
    crate::ui::chrome::init(cx);
    crate::window_state::init(cx);
    crate::terminal::init(cx);
    crate::agents::init(cx);
    crate::automations::init(cx);
    crate::dragdrop::init(cx);
    crate::ports::init(cx);
}
