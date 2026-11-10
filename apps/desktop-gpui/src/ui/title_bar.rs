//! The window's own title bar, the same size as the Tauri one. It scales, except
//! on macOS, where it is sized around the fixed traffic lights.
//!
//! With tabs in the bar it is as tall as the tab strip it replaces, and the
//! window controls lead instead of trailing.
//!
//! Kit's `TitleBar` is not used because its window controls carry no AccessKit
//! role. T2.5 owns Linux decoration selection and the macOS window details.

use gpui_kit::{
    AnyElement, App, Decorations, DefiniteLength, InteractiveElement, IntoElement, MouseButton,
    ParentElement, RenderOnce, Rgba, Role, SharedString, StatefulInteractiveElement, Styled,
    Window,
    component::{
        ActiveTheme, Icon, IconName, InteractiveElementExt, Sizable,
        button::{Button, ButtonVariants},
        h_flex,
    },
    div,
    prelude::FluentBuilder,
    px,
};

use crate::theme::{
    self,
    metrics::{BAR_HEIGHT, TITLEBAR_HEIGHT, ui},
};

/// Height and left inset. macOS draws its traffic lights over the bar at a fixed
/// size, so there the room kept for them does not follow UI scale. With tabs in
/// the bar, the lead section sets its own inset.
fn bar_geometry(tabs_in_bar: bool) -> (DefiniteLength, DefiniteLength) {
    match (cfg!(target_os = "macos"), tabs_in_bar) {
        (true, true) => (px(38.).into(), px(0.).into()),
        (true, false) => (px(38.).into(), px(78.).into()),
        (false, true) => (ui(BAR_HEIGHT).into(), ui(0.).into()),
        (false, false) => (ui(TITLEBAR_HEIGHT).into(), ui(6.).into()),
    }
}

#[derive(IntoElement)]
pub struct TitleBar {
    content: AnyElement,
    tabs_in_bar: bool,
}

impl TitleBar {
    pub fn new(content: AnyElement, tabs_in_bar: bool) -> Self {
        Self {
            content,
            tabs_in_bar,
        }
    }
}

/// Whether this window gets its own minimize, maximize and close.
pub fn should_show_window_controls(window: &Window) -> bool {
    // A tiled window is the compositor's to manage. GPUI also reports a
    // maximized window as tiled, and that one keeps its controls.
    cfg!(target_os = "linux")
        && matches!(
            window.window_decorations(),
            Decorations::Client { tiling } if window.is_maximized() || !tiling.is_tiled()
        )
}

/// Starts a window move on the first drag after a press on the bar.
struct DragState {
    should_move: bool,
}

impl RenderOnce for TitleBar {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let drag = window.use_state(cx, |_, _| DragState { should_move: false });
        let show_controls = should_show_window_controls(window);
        let (height, left_inset) = bar_geometry(self.tabs_in_bar);
        let tabs_in_bar = self.tabs_in_bar;

        h_flex()
            .id("title-bar")
            .role(Role::TitleBar)
            .aria_label("Title bar")
            .flex_shrink_0()
            .items_center()
            .justify_between()
            .h(height)
            .pl(left_inset)
            // Tabs in the bar keep the controls in the content's lead.
            .pr(if tabs_in_bar || show_controls {
                ui(0.)
            } else {
                ui(6.)
            })
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
                    .h_full()
                    .min_w_0()
                    .overflow_hidden()
                    .child(self.content),
            )
            .when(show_controls && !tabs_in_bar, |this| {
                this.child(WindowControls::trailing())
            })
    }
}

/// A `.bar-button`: an icon in the ink its state calls for. The icon carries the
/// color because Kit's disabled style would otherwise replace it.
pub fn bar_button(
    id: &'static str,
    icon: impl Into<Icon>,
    label: impl Into<SharedString>,
    ink: Rgba,
) -> Button {
    Button::new(id)
        .ghost()
        .xsmall()
        .compact()
        .w(ui(22.))
        .h(ui(20.))
        .icon(Icon::new(icon).size(ui(14.)).text_color(ink))
        .accessibility_label(label)
}

/// A bar button's ink while disabled: faint, and dimmed as Tauri's
/// `button:disabled` is. An `aria-disabled` control keeps plain faint.
pub fn disabled_ink(faint: Rgba) -> Rgba {
    Rgba { a: 0.42, ..faint }
}

/// Minimize, maximize and close, for client-decorated Linux windows. Close leads
/// when the controls sit at the start of the bar.
#[derive(IntoElement)]
pub struct WindowControls {
    leading: bool,
}

impl WindowControls {
    pub fn trailing() -> Self {
        Self { leading: false }
    }

    pub fn leading() -> Self {
        Self { leading: true }
    }
}

impl RenderOnce for WindowControls {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let ink = theme::palette(cx).chrome_dim;
        let (maximize_icon, maximize_label) = if window.is_maximized() {
            (IconName::WindowRestore, "Restore window")
        } else {
            (IconName::WindowMaximize, "Maximize window")
        };
        let leading = self.leading;
        let control = |id: &'static str, icon: IconName, label: &'static str| {
            bar_button(id, icon, label, ink)
        };
        let minimize = control(
            "window-minimize",
            IconName::WindowMinimize,
            "Minimize window",
        )
        .on_click(|_, window, _| window.minimize_window());
        let maximize = control("window-maximize", maximize_icon, maximize_label)
            .on_click(|_, window, _| window.zoom_window());
        let close = control("window-close", IconName::WindowClose, "Close window")
            .on_click(|_, window, _| window.remove_window());
        h_flex()
            .id("window-controls")
            .h_full()
            .flex_none()
            .items_center()
            .gap(ui(2.))
            .when(leading, |this| this.mr(ui(8.)))
            .when(!leading, |this| this.ml(ui(8.)).pr(ui(6.)))
            // A press on a control is a press, not the start of a window drag.
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .map(|this| {
                if leading {
                    this.child(close).child(minimize).child(maximize)
                } else {
                    this.child(minimize).child(maximize).child(close)
                }
            })
    }
}
