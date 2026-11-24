//! The window's own title bar, the same size as the Tauri one. It scales, except
//! on macOS, where it is sized around the fixed traffic lights.
//!
//! With tabs in the bar it is as tall as the tab strip it replaces.
//!
//! Kit's `TitleBar` is not used because its window controls carry no AccessKit
//! role. T2.5 owns Linux decoration selection and the macOS window details.

use std::{mem, sync::OnceLock};

use gpui_kit::{
    App, Decorations, DefiniteLength, Entity, Hsla, InteractiveElement, IntoElement,
    MAX_BUTTONS_PER_SIDE, MouseButton, MouseMoveEvent, MouseUpEvent, ParentElement, RenderOnce,
    Rgba, Role, SharedString, StatefulInteractiveElement, Styled, Window, WindowButton,
    WindowButtonLayout, canvas,
    component::{
        ActiveTheme, Icon, InteractiveElementExt, Sizable,
        button::{Button, ButtonCustomVariant, ButtonVariants},
        h_flex,
    },
    div,
    prelude::FluentBuilder,
    px,
};

use crate::menus::menu_button;
use crate::theme::{
    self,
    metrics::{BAR_HEIGHT, TITLEBAR_HEIGHT, ui},
};
use crate::ui::{chrome::TitleContent, icons::Glyph};

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
pub(super) struct TitleBar {
    content: TitleContent,
    tabs_in_bar: bool,
}

impl TitleBar {
    pub(super) fn new(content: TitleContent, tabs_in_bar: bool) -> Self {
        Self {
            content,
            tabs_in_bar,
        }
    }
}

/// Who draws the window buttons on Linux. Port of `linux_window.rs`, minus its
/// two environment overrides, which the owner removed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChromeMode {
    /// The bar draws the window controls.
    Buttons,
    /// No buttons: a tiling compositor manages the window, and a close button
    /// there is noise.
    Bare,
}

/// Desktops whose compositor tiles windows, matched against each
/// colon-separated entry of `XDG_CURRENT_DESKTOP`.
const TILING_DESKTOPS: &[&str] = &[
    "hyprland", "sway", "niri", "river", "i3", "qtile", "bspwm", "dwm",
];

/// The decision from `XDG_CURRENT_DESKTOP`. Unknown desktops get buttons.
pub fn classify(desktop: Option<&str>) -> ChromeMode {
    let tiling = desktop
        .unwrap_or_default()
        .split(':')
        .any(|entry| TILING_DESKTOPS.contains(&entry.trim().to_ascii_lowercase().as_str()));
    if tiling {
        ChromeMode::Bare
    } else {
        ChromeMode::Buttons
    }
}

/// The `window.chrome` incident Tauri journaled at start on Linux: what was
/// decided, on which desktop, and why. A floating window without buttons
/// would be one nobody can close, so the decision is worth a line.
pub fn chrome_incident(desktop: Option<&str>) -> serde_json::Value {
    let (mode, reason) = match classify(desktop) {
        ChromeMode::Buttons => ("buttons", "floating desktop"),
        ChromeMode::Bare => ("bare", "tiling desktop"),
    };
    serde_json::json!({ "mode": mode, "desktop": desktop.unwrap_or_default(), "reason": reason })
}

fn chrome_mode() -> ChromeMode {
    static MODE: OnceLock<ChromeMode> = OnceLock::new();
    *MODE.get_or_init(|| classify(std::env::var("XDG_CURRENT_DESKTOP").ok().as_deref()))
}

/// Whether this window gets its own minimize, maximize and close. A window the
/// compositor decorates already has its own.
pub fn should_show_window_controls(window: &Window) -> bool {
    cfg!(target_os = "linux")
        && chrome_mode() == ChromeMode::Buttons
        && matches!(window.window_decorations(), Decorations::Client { .. })
}

/// Starts a window move on the first pointer move after a press on the bar.
struct DragState {
    should_move: bool,
}

/// Listens for the move and the release on the whole window. A listener on the
/// bar only hears moves while the pointer is over it, so a quick drag that left
/// the bar before its first move event never started the move, and a release
/// outside the bar was never seen.
fn drag_tracker(drag: Entity<DragState>) -> impl IntoElement {
    canvas(
        |_, _, _| {},
        move |_, _, window, _| {
            let moved = drag.clone();
            window.on_mouse_event(move |_: &MouseMoveEvent, _, window, cx| {
                if moved.update(cx, |drag, _| mem::take(&mut drag.should_move)) {
                    window.start_window_move();
                }
            });
            let released = drag.clone();
            window.on_mouse_event(move |_: &MouseUpEvent, _, _, cx| {
                released.update(cx, |drag, _| drag.should_move = false);
            });
        },
    )
    .absolute()
}

impl RenderOnce for TitleBar {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        self.render_with_layout(
            should_show_window_controls(window),
            cx.button_layout(),
            window,
            cx,
        )
    }
}

fn fallback_layout(tabs_in_bar: bool) -> WindowButtonLayout {
    use WindowButton::{Close, Maximize, Minimize};

    if tabs_in_bar {
        WindowButtonLayout {
            left: [Some(Close), Some(Minimize), Some(Maximize)],
            right: [None; MAX_BUTTONS_PER_SIDE],
        }
    } else {
        WindowButtonLayout {
            left: [None; MAX_BUTTONS_PER_SIDE],
            right: [Some(Minimize), Some(Maximize), Some(Close)],
        }
    }
}

impl TitleBar {
    fn render_with_layout(
        self,
        show_controls: bool,
        desktop_layout: Option<WindowButtonLayout>,
        window: &mut Window,
        cx: &mut App,
    ) -> impl IntoElement {
        let Self {
            mut content,
            tabs_in_bar,
        } = self;
        let layout =
            show_controls.then(|| desktop_layout.unwrap_or_else(|| fallback_layout(tabs_in_bar)));
        let mut leading = layout.and_then(|layout| WindowControls::new(layout.left, true));
        let trailing = layout.and_then(|layout| WindowControls::new(layout.right, false));
        if tabs_in_bar {
            // Keep the tabs aligned with the sidebar when there are left-side controls.
            content.leading_controls = leading.take();
            content.has_trailing_window_controls = trailing.is_some();
        }

        let drag = window.use_state(cx, |_, _| DragState { should_move: false });
        let (height, left_inset) = bar_geometry(tabs_in_bar);

        h_flex()
            .id("title-bar")
            .role(Role::TitleBar)
            .aria_label("Title bar")
            .flex_shrink_0()
            .items_center()
            .justify_between()
            .h(height)
            .pl(left_inset)
            .pr(if tabs_in_bar || trailing.is_some() {
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
            .on_mouse_down(
                MouseButton::Left,
                // A button on the bar prevents the default of its press, and a click on it is not a drag.
                window.listener_for(&drag, |drag, _, window, _| {
                    drag.should_move = !window.default_prevented();
                }),
            )
            .child(drag_tracker(drag))
            .children(leading)
            .child(
                div()
                    .flex_1()
                    .h_full()
                    .min_w_0()
                    .overflow_hidden()
                    .child(content),
            )
            .when(cfg!(target_os = "linux"), |this| {
                this.child(menu_button(cx))
            })
            .children(trailing)
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

/// One side of the window controls for client-decorated Linux windows.
#[derive(IntoElement)]
pub(super) struct WindowControls {
    buttons: [Option<WindowButton>; MAX_BUTTONS_PER_SIDE],
    leading: bool,
}

impl WindowControls {
    fn new(buttons: [Option<WindowButton>; MAX_BUTTONS_PER_SIDE], leading: bool) -> Option<Self> {
        buttons
            .iter()
            .any(Option::is_some)
            .then_some(Self { buttons, leading })
    }
}

fn window_button(button: WindowButton, window: &mut Window, cx: &mut App) -> Button {
    let palette = theme::palette(cx);
    let ink = palette.chrome_dim;
    match button {
        WindowButton::Minimize => bar_button("window-minimize", Glyph::Minus, "Minimize", ink)
            .on_click(|_, window, _| window.minimize_window()),
        WindowButton::Maximize => {
            let (icon, label) = if window.is_maximized() {
                (Glyph::WindowRestore, "Restore")
            } else {
                (Glyph::WindowMaximize, "Maximize")
            };
            bar_button("window-maximize", icon, label, ink)
                .on_click(|_, window, _| window.zoom_window())
        }
        // Close is the one control that answers hover with the danger color, and its
        // icon brightens with it.
        WindowButton::Close => {
            let close_hovered = window.use_state(cx, |_, _| false);
            let danger = Hsla::from(palette.danger);
            let close_ink = if *close_hovered.read(cx) {
                palette.chrome_ink_strong
            } else {
                ink
            };
            bar_button("window-close", Glyph::Close, "Close window", close_ink)
                .custom(ButtonCustomVariant::new(cx).hover(danger).active(danger))
                .on_hover(move |hovered, _, cx| {
                    close_hovered.update(cx, |state, cx| {
                        *state = *hovered;
                        cx.notify();
                    })
                })
                // Through the editors' close guard, as a platform close request is.
                .on_click(|_, window, cx| crate::editor::close_window(window, cx))
        }
    }
}

impl RenderOnce for WindowControls {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let leading = self.leading;
        let id = if leading {
            "window-controls-left"
        } else {
            "window-controls-right"
        };
        let group = h_flex()
            .id(id)
            .h_full()
            .flex_none()
            .items_center()
            .gap(ui(2.))
            .when(leading, |this| this.mr(ui(8.)))
            .when(!leading, |this| this.ml(ui(8.)).pr(ui(6.)))
            // A press on a control is a press, not the start of a window drag.
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation());
        // Give the close button's hover state the same side as the group.
        window.with_element_namespace(id, |window| {
            group.children(
                self.buttons
                    .into_iter()
                    .flatten()
                    .map(|button| window_button(button, window, cx)),
            )
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "linux")]
    mod placement {
        use desktop_core::app_state::PersistedAppState;
        use gpui_kit::{
            Context, Render, TestAppContext, VisualTestContext, size, test::TestQueryExt,
        };

        use super::*;
        use crate::{app, ui::chrome::Chrome};

        struct TestBar {
            chrome: Entity<Chrome>,
            layout: Option<WindowButtonLayout>,
        }

        impl Render for TestBar {
            fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
                let chrome = self.chrome.read(cx);
                let bar = TitleBar::new(chrome.title_bar_content(), chrome.tabs_in_title_bar());
                div()
                    .size_full()
                    .child(bar.render_with_layout(true, self.layout, window, cx))
            }
        }

        fn check_layouts(tabs_in_bar: bool, cx: &mut TestAppContext) {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("app-state.json");
            let mut state = PersistedAppState::default();
            state.shell.tabs_in_title_bar = Some(tabs_in_bar);
            std::fs::write(&path, serde_json::to_vec(&state).unwrap()).unwrap();
            cx.update(|cx| {
                gpui_kit::init(cx);
                app::init(app::Core::at(&path), cx);
            });
            let (bar, visual) = cx.add_window_view(|window, cx| {
                let chrome = Chrome::new(window, cx);
                assert_eq!(chrome.read(cx).tabs_in_title_bar(), tabs_in_bar);
                TestBar {
                    chrome,
                    layout: None,
                }
            });
            const CLOSE: &str = "Close window";
            const MAXIMIZE: &str = "Maximize";
            const MINIMIZE: &str = "Minimize";
            let cases: [(Option<&str>, &[&str], &[&str]); 6] = [
                (
                    Some("close,maximize,minimize:"),
                    &[CLOSE, MAXIMIZE, MINIMIZE],
                    &[],
                ),
                (
                    Some("appmenu:maximize,minimize,close"),
                    &[],
                    &[MAXIMIZE, MINIMIZE, CLOSE],
                ),
                (
                    Some("minimize,close:maximize"),
                    &[MINIMIZE, CLOSE],
                    &[MAXIMIZE],
                ),
                (Some(":close"), &[], &[CLOSE]),
                (Some(":"), &[], &[]),
                if tabs_in_bar {
                    (None, &[CLOSE, MINIMIZE, MAXIMIZE], &[])
                } else {
                    (None, &[], &[MINIMIZE, MAXIMIZE, CLOSE])
                },
            ];
            for width in [1280., app::MIN_WINDOW_WIDTH] {
                visual.simulate_resize(size(px(width), px(800.)));
                for (layout, left, right) in cases {
                    visual.update(|_, cx| {
                        bar.update(cx, |bar, cx| {
                            bar.layout =
                                layout.map(|layout| WindowButtonLayout::parse(layout).unwrap());
                            cx.notify();
                        });
                    });
                    visual.run_until_parked();
                    assert_placement(visual, layout, left, right, tabs_in_bar, width);
                }
            }
        }

        fn assert_placement(
            visual: &mut VisualTestContext,
            layout: Option<&str>,
            left: &[&str],
            right: &[&str],
            tabs_in_bar: bool,
            width: f32,
        ) {
            visual.update(|window, _| {
                let mut labels = left.to_vec();
                labels.extend(["Settings", "Toggle sidebar"]);
                if tabs_in_bar && width > app::MIN_WINDOW_WIDTH {
                    labels.insert(left.len() + 1, "New terminal tab");
                }
                labels.extend(["Toggle right panel", "Main menu"]);
                labels.extend_from_slice(right);
                let bounds: Vec<_> = labels
                    .iter()
                    .map(|label| window.find_by_label(label).bounds())
                    .collect();
                for (pair, labels) in bounds.windows(2).zip(labels.windows(2)) {
                    assert!(
                        pair[0].right() <= pair[1].left(),
                        "{layout:?} at {width}px: {} {:?} overlaps or follows {} {:?}",
                        labels[0],
                        pair[0],
                        labels[1],
                        pair[1],
                    );
                }
                for label in ["Close window", "Maximize", "Minimize"] {
                    assert_eq!(
                        window.try_find_by_label(label).map(|button| button.role()),
                        (left.contains(&label) || right.contains(&label))
                            .then_some(Some(Role::Button)),
                        "{layout:?}: {label}"
                    );
                }
            });
        }

        #[gpui_kit::test]
        fn plain_title_bar_follows_the_desktop_layout(cx: &mut TestAppContext) {
            check_layouts(false, cx);
        }

        #[gpui_kit::test]
        fn tabbed_title_bar_follows_the_desktop_layout(cx: &mut TestAppContext) {
            check_layouts(true, cx);
        }
    }

    #[test]
    fn the_journal_line_says_what_decided() {
        assert_eq!(
            chrome_incident(Some("Hyprland")),
            serde_json::json!({ "mode": "bare", "desktop": "Hyprland", "reason": "tiling desktop" })
        );
        assert_eq!(
            chrome_incident(None),
            serde_json::json!({ "mode": "buttons", "desktop": "", "reason": "floating desktop" })
        );
    }

    #[test]
    fn floating_and_unknown_desktops_get_buttons() {
        for desktop in [
            None,
            Some(""),
            Some("GNOME"),
            Some("KDE"),
            Some("X-Cinnamon"),
        ] {
            assert_eq!(classify(desktop), ChromeMode::Buttons, "{desktop:?}");
        }
    }

    #[test]
    fn tiling_desktops_are_bare_however_they_are_listed() {
        for desktop in [
            "Hyprland",
            "sway",
            "ubuntu:i3",
            "Niri",
            "something:Sway:else",
        ] {
            assert_eq!(classify(Some(desktop)), ChromeMode::Bare, "{desktop}");
        }
    }

    #[test]
    fn a_desktop_that_merely_contains_a_tiling_name_is_not_one() {
        assert_eq!(classify(Some("swayfx-fork")), ChromeMode::Buttons);
    }
}
