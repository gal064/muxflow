//! The window's own title bar, the same size as the Tauri one. It scales, except
//! on macOS, where it is sized around the fixed traffic lights.
//!
//! Kit's `TitleBar` is not used because its window controls carry no AccessKit
//! role. T2.2 owns its height and control placement for tabs in the title bar.
//! T2.5 owns Linux decoration selection and the macOS window details.

use gpui_kit::{
    AnyElement, App, Decorations, DefiniteLength, InteractiveElement, IntoElement, MouseButton,
    ParentElement, RenderOnce, Role, StatefulInteractiveElement, Styled, Window,
    component::{
        ActiveTheme, IconName, InteractiveElementExt, Sizable,
        button::{Button, ButtonVariants},
        h_flex,
    },
    div,
    prelude::FluentBuilder,
    px,
};

use crate::theme::metrics::{TITLEBAR_HEIGHT, ui};

/// Height and left inset. macOS draws its traffic lights over the bar at a fixed
/// size, so there the room kept for them does not follow UI scale.
fn bar_geometry() -> (DefiniteLength, DefiniteLength) {
    if cfg!(target_os = "macos") {
        (px(38.).into(), px(78.).into())
    } else {
        (ui(TITLEBAR_HEIGHT).into(), ui(12.).into())
    }
}

#[derive(IntoElement)]
pub struct TitleBar {
    content: AnyElement,
}

impl TitleBar {
    pub fn new(content: AnyElement) -> Self {
        Self { content }
    }
}

/// Starts a window move on the first drag after a press on the bar.
struct DragState {
    should_move: bool,
}

impl RenderOnce for TitleBar {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let drag = window.use_state(cx, |_, _| DragState { should_move: false });
        // A tiled window is the compositor's to manage. GPUI also reports a
        // maximized window as tiled, and that one keeps its controls.
        let show_controls = cfg!(target_os = "linux")
            && matches!(
                window.window_decorations(),
                Decorations::Client { tiling } if window.is_maximized() || !tiling.is_tiled()
            );
        let (height, left_inset) = bar_geometry();

        h_flex()
            .id("title-bar")
            .role(Role::TitleBar)
            .aria_label("Title bar")
            .flex_shrink_0()
            .items_center()
            .justify_between()
            .h(height)
            .pl(left_inset)
            .border_b_1()
            .border_color(cx.theme().title_bar_border)
            .bg(cx.theme().title_bar)
            .when(cfg!(target_os = "macos"), |this| {
                this.on_double_click(|_, window, _| window.titlebar_double_click())
            })
            .when(cfg!(target_os = "linux"), |this| {
                this.on_double_click(|_, window, _| window.zoom_window())
            })
            .on_mouse_down_out(window.listener_for(&drag, |drag, _, _, _| {
                drag.should_move = false;
            }))
            .on_mouse_down(
                MouseButton::Left,
                window.listener_for(&drag, |drag, _, _, _| drag.should_move = true),
            )
            .on_mouse_up(
                MouseButton::Left,
                window.listener_for(&drag, |drag, _, _, _| drag.should_move = false),
            )
            .on_mouse_move(window.listener_for(&drag, |drag, _, window, _| {
                if drag.should_move {
                    drag.should_move = false;
                    window.start_window_move();
                }
            }))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .child(self.content),
            )
            .when(show_controls, |this| this.child(WindowControls))
    }
}

/// Minimize, maximize and close, for client-decorated Linux windows.
#[derive(IntoElement)]
pub struct WindowControls;

impl RenderOnce for WindowControls {
    fn render(self, window: &mut Window, _: &mut App) -> impl IntoElement {
        let (maximize_icon, maximize_label) = if window.is_maximized() {
            (IconName::WindowRestore, "Restore window")
        } else {
            (IconName::WindowMaximize, "Maximize window")
        };
        let control = |id: &'static str, icon: IconName, label: &'static str| {
            Button::new(id)
                .ghost()
                .xsmall()
                .compact()
                .icon(icon)
                .accessibility_label(label)
        };
        h_flex()
            .id("window-controls")
            .h_full()
            .items_center()
            .gap(ui(2.))
            .ml(ui(8.))
            .pr(ui(6.))
            // A press on a control is a press, not the start of a window drag.
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(
                control(
                    "window-minimize",
                    IconName::WindowMinimize,
                    "Minimize window",
                )
                .on_click(|_, window, _| window.minimize_window()),
            )
            .child(
                control("window-maximize", maximize_icon, maximize_label)
                    .on_click(|_, window, _| window.zoom_window()),
            )
            .child(
                control("window-close", IconName::WindowClose, "Close window")
                    .on_click(|_, window, _| window.remove_window()),
            )
    }
}
