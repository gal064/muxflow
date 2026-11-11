//! What the title bar shows, a port of `features/shell/TitleBar.tsx`. With the
//! tabs in the bar it splits into the app's own controls over the sidebar and
//! the tabs over the workspace; without them it is one full-width bar.

use gpui_kit::{
    AnyElement, App, FontWeight, InteractiveElement, IntoElement, MouseButton, ParentElement,
    RenderOnce, SharedString, Styled, WeakEntity, Window, assets::IconName, base::Disableable,
    component::h_flex, div, prelude::FluentBuilder, px,
};

use crate::{
    commands,
    theme::{
        self,
        metrics::{TYPE_TITLEBAR, ui},
    },
    tooltips::with_shortcut,
    ui::{
        chrome::{Chrome, tab_strip::TabStrip},
        title_bar::{WindowControls, bar_button, disabled_ink, should_show_window_controls},
    },
    workspaces::focus_history::Direction,
};

/// A width the macOS traffic lights take from the lead; not UI-scaled.
const TRAFFIC_LIGHT_INSET: f32 = 78.;

#[derive(IntoElement)]
pub struct TitleContent(pub WeakEntity<Chrome>);

impl RenderOnce for TitleContent {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let palette = theme::palette(cx);
        let Some(entity) = self.0.upgrade() else {
            return div().into_any_element();
        };
        let chrome = entity.read(cx);
        let rails = chrome.rails(window);
        let in_title_bar = chrome.tabs_in_title_bar();
        let workspaces = chrome.workspaces().clone();
        let (name, can_back, can_forward, can_create) = {
            let state = workspaces.read(cx);
            let name = state
                .view(cx)
                .session
                .map_or_else(|| "No workspace".to_owned(), |session| session.name);
            let can_create = state
                .hosts(cx)
                .iter()
                .any(|host| host.shown && host.can_mutate);
            (
                name,
                state.can_go(Direction::Back, cx),
                state.can_go(Direction::Forward, cx),
                can_create,
            )
        };

        let disabled_button = |id: &'static str, icon: IconName, label: &'static str| {
            bar_button(id, icon, label, disabled_ink(palette.chrome_faint)).disabled(true)
        };

        let tip = |label: &str, command| with_shortcut(label, command, cx);
        let (settings_tip, back_tip, forward_tip, new_workspace_tip) = (
            tip("Settings", "settings.show"),
            tip("Back", "focus.back"),
            tip("Forward", "focus.forward"),
            tip("New workspace", "session.new"),
        );
        let (sidebar_tip, panel_tip) = (
            tip("Toggle sidebar", "view.toggleSidebar"),
            tip("Toggle right panel", "view.togglePanel"),
        );
        let settings = || {
            disabled_button("title-settings", IconName::Settings, "Settings")
                .tooltip(settings_tip.clone())
        };
        // Three states, and the tooltip says what the label does: somewhere to
        // go, nothing waiting, or something waiting the bell cannot reach.
        let (unread, can_jump) = crate::agents::bell(cx);
        let plural = if unread == 1 { "" } else { "s" };
        let hint: SharedString = if can_jump {
            "Go to the next agent waiting (blocked first, then unread completed)".into()
        } else if unread > 0 {
            format!("{unread} agent{plural} waiting, none reachable right now").into()
        } else {
            "No agents waiting".into()
        };
        let label: SharedString = if can_jump {
            format!("{unread} agent{plural} waiting; go to the next one").into()
        } else {
            hint.clone()
        };
        let bell = || {
            // `aria-disabled` in Tauri: faint, without the disabled dimming.
            let ink = if can_jump {
                palette.chrome_dim
            } else {
                palette.chrome_faint
            };
            div()
                .relative()
                .child(
                    bar_button("title-bell", IconName::Bell, label.clone(), ink)
                        .tooltip(hint.clone())
                        .disabled(!can_jump)
                        .on_click(|_, _, cx| crate::agents::jump_to_unread(cx)),
                )
                .when(unread > 0, |this| {
                    this.child(
                        div()
                            .absolute()
                            .top(ui(-1.))
                            .right(ui(-3.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .min_w(ui(15.))
                            .h(ui(15.))
                            .px(ui(4.))
                            .rounded(ui(8.))
                            .bg(palette.accent)
                            .text_color(palette.accent_ink)
                            .text_size(ui(9.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(if unread > 99 {
                                "99+".to_owned()
                            } else {
                                unread.to_string()
                            }),
                    )
                })
        };
        let command_button =
            |id: &'static str, icon: IconName, label: &'static str, enabled: bool, command| {
                bar_button(
                    id,
                    icon,
                    label,
                    if enabled {
                        palette.chrome_dim
                    } else {
                        disabled_ink(palette.chrome_faint)
                    },
                )
                .disabled(!enabled)
                .on_click(move |_, window, cx| commands::run(command, window, cx))
            };
        let back = || {
            command_button(
                "title-back",
                IconName::ArrowLeft,
                "Back",
                can_back,
                "focus.back",
            )
            .tooltip(back_tip.clone())
        };
        let forward = || {
            command_button(
                "title-forward",
                IconName::ArrowRight,
                "Forward",
                can_forward,
                "focus.forward",
            )
            .tooltip(forward_tip.clone())
        };
        let new_workspace = || {
            command_button(
                "title-new-workspace",
                IconName::Plus,
                "New workspace",
                can_create,
                "session.new",
            )
            .tooltip(new_workspace_tip.clone())
        };

        let rail_ink = move |open: bool, fits: bool| {
            if !fits {
                palette.chrome_faint
            } else if open {
                palette.chrome_ink_strong
            } else {
                palette.chrome_dim
            }
        };

        let sidebar_toggle = {
            let weak = self.0.clone();
            move || {
                let weak = weak.clone();
                bar_button(
                    "title-sidebar-toggle",
                    IconName::PanelLeft,
                    "Toggle sidebar",
                    rail_ink(rails.sidebar_open, rails.sidebar_fits),
                )
                .toggled(rails.sidebar_open)
                .tooltip(if rails.sidebar_fits {
                    sidebar_tip.clone()
                } else {
                    "Widen the window to show the sidebar".into()
                })
                .disabled(!rails.sidebar_fits)
                .on_click(move |_, _, cx| {
                    weak.update(cx, |chrome, cx| chrome.toggle_sidebar(cx)).ok();
                })
            }
        };
        let panel_toggle = {
            let weak = self.0.clone();
            move || {
                let weak = weak.clone();
                bar_button(
                    "title-panel-toggle",
                    IconName::PanelRight,
                    "Toggle right panel",
                    rail_ink(rails.panel_open, rails.panel_fits),
                )
                .toggled(rails.panel_open)
                .tooltip(if rails.panel_fits {
                    panel_tip.clone()
                } else {
                    "Widen the window to show Files and Git".into()
                })
                .disabled(!rails.panel_fits)
                .on_click(move |_, _, cx| {
                    weak.update(cx, |chrome, cx| chrome.toggle_panel(cx)).ok();
                })
            }
        };

        // A control press is a press, not the start of a window drag.
        let group = || {
            h_flex()
                .h_full()
                .flex_none()
                .items_center()
                .gap(ui(2.))
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        };

        let content: AnyElement = if in_title_bar {
            let macos_inset = cfg!(target_os = "macos");
            let lead = h_flex()
                .h_full()
                .flex_none()
                .items_center()
                .gap(ui(2.))
                .px(ui(6.))
                .when(macos_inset, |this| this.pl(px(TRAFFIC_LIGHT_INSET)))
                .when(rails.sidebar_open, |this| {
                    this.min_w(ui(rails.sidebar_width))
                })
                .when(should_show_window_controls(window), |this| {
                    this.child(WindowControls::leading())
                })
                .child(
                    group()
                        .child(settings())
                        .child(bell())
                        .child(back())
                        .child(forward()),
                )
                .when(!rails.sidebar_open, |this| {
                    this.child(group().child(new_workspace()))
                });

            let strip = TabStrip {
                workspaces,
                in_title_bar: true,
            };

            h_flex()
                .w_full()
                .h_full()
                .min_w_0()
                .items_center()
                .child(lead)
                .child(
                    div()
                        .flex_1()
                        .h_full()
                        .min_w_0()
                        .overflow_hidden()
                        .child(strip),
                )
                // Fixed, so a window full of tabs still leaves room to drag it.
                .child(div().flex_none().w(ui(32.)).h_full())
                .child(
                    group()
                        .pl(ui(4.))
                        .pr(ui(6.))
                        .border_l_1()
                        .border_color(palette.chrome_hairline)
                        .child(sidebar_toggle())
                        .child(panel_toggle()),
                )
                .into_any_element()
        } else {
            h_flex()
                .w_full()
                .h_full()
                .min_w_0()
                .items_center()
                .gap(ui(2.))
                .child(
                    group()
                        .child(settings())
                        .child(sidebar_toggle())
                        .child(back())
                        .child(forward()),
                )
                .child(
                    div()
                        .ml(ui(6.))
                        .min_w_0()
                        .truncate()
                        .text_size(ui(TYPE_TITLEBAR))
                        .font_weight(FontWeight(550.))
                        .text_color(palette.chrome_ink)
                        .child(name),
                )
                // The empty space: the window drags from here.
                .child(div().flex_1().min_w(ui(8.)).h_full())
                .child(
                    group()
                        .child(bell())
                        .child(new_workspace())
                        .child(panel_toggle()),
                )
                .into_any_element()
        };
        content
    }
}
