//! What the title bar shows, a port of `features/shell/TitleBar.tsx`. With the
//! tabs in the bar it splits into the app's own controls over the sidebar and
//! the tabs over the workspace; without them it is one full-width bar.

use gpui_kit::{
    AnyElement, App, FontWeight, InteractiveElement, IntoElement, MouseButton, ParentElement,
    RenderOnce, Styled, WeakEntity, Window, assets::IconName, base::Disableable, component::h_flex,
    div, prelude::FluentBuilder, px,
};

use crate::{
    theme::{
        self,
        metrics::{TYPE_TITLEBAR, ui},
    },
    ui::{
        chrome::{Chrome, tab_strip::TabStrip},
        title_bar::{WindowControls, bar_button, disabled_ink, should_show_window_controls},
    },
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
        let workspace = chrome.workspace().cloned();
        let name = workspace
            .as_ref()
            .map_or("No workspace".into(), |workspace| workspace.name.clone());

        let disabled_button = |id: &'static str, icon: IconName, label: &'static str| {
            bar_button(id, icon, label, disabled_ink(palette.chrome_faint)).disabled(true)
        };

        let settings = || disabled_button("title-settings", IconName::Settings, "Settings");
        // `aria-disabled` in Tauri: faint, without the disabled dimming.
        let bell = || {
            bar_button(
                "title-bell",
                IconName::Bell,
                "No agents waiting",
                palette.chrome_faint,
            )
            .disabled(true)
        };
        let back = || disabled_button("title-back", IconName::ArrowLeft, "Back");
        let forward = || disabled_button("title-forward", IconName::ArrowRight, "Forward");
        let new_workspace =
            || disabled_button("title-new-workspace", IconName::Plus, "New workspace");

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

            let tabs = workspace.as_ref();
            let strip = TabStrip {
                chrome: self.0.clone(),
                tabs: tabs.map_or_else(Vec::new, |w| w.tabs.clone()),
                active: tabs.and_then(|w| w.active_tab.clone()),
                docked: tabs.and_then(|w| w.docked.as_ref().map(|(key, _)| key.clone())),
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
