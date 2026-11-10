//! Owned by T2.2 `chrome-layout`: the window body (dock, sidebar, tabs, right
//! panel) and what the title bar shows. Everything goes under `ui/chrome/`.
//!
//! [`Chrome`] is the seam [`crate::ui::shell::Shell`] renders; replace its
//! contents, not the `Shell`.

use gpui_kit::{
    App, Context, Entity, FontWeight, IntoElement, Render, Role, Window, component::ActiveTheme,
    div, prelude::*,
};

use crate::theme::metrics::{TYPE_TITLEBAR, ui};

/// Called once from [`crate::features::init`].
pub fn init(_cx: &mut App) {}

pub struct Chrome;

impl Chrome {
    pub fn new(_window: &mut Window, cx: &mut App) -> Entity<Self> {
        cx.new(|_| Self)
    }

    /// The title bar's content, to the left of the window controls. A press
    /// that reaches the bar starts a window drag, so an interactive child calls
    /// `cx.stop_propagation()` on left mouse-down, as `WindowControls` does.
    pub fn title_bar_content(&self) -> impl IntoElement {
        div()
            .text_size(ui(TYPE_TITLEBAR))
            .font_weight(FontWeight(550.))
            .child("Muxflow")
    }
}

impl Render for Chrome {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .id("workspace")
            .role(Role::Main)
            .aria_label("Workspace")
            .size_full()
            .bg(cx.theme().background)
    }
}
