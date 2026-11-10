//! The 240px rail: workspaces on top, agents below with a divider between them,
//! and the host row at the bottom. A port of the layout of
//! `features/workspaces/WorkspaceSidebar.tsx`; rows are placeholders until host
//! data arrives.

use desktop_core::app_state::AgentSortMode;
use gpui_kit::{
    App, FontWeight, InteractiveElement, IntoElement, ParentElement, RenderOnce, Role,
    SharedString, StatefulInteractiveElement, Styled, WeakEntity, Window,
    assets::IconName,
    base::{Disableable, HandleEdge, TestSupportExt},
    component::{
        Sizable,
        button::{Button, ButtonVariants},
        h_flex, v_flex,
    },
    div,
    prelude::FluentBuilder,
    relative,
};

use crate::{
    fonts,
    theme::{
        self,
        metrics::{RADIUS_ROW, TYPE_LABEL, TYPE_META, TYPE_ROW, TYPE_SECONDARY, WEIGHT_ROW, ui},
    },
    ui::{
        chrome::{
            Chrome,
            divider::{Divider, divider},
            layout,
        },
        title_bar::{bar_button, disabled_ink},
    },
};

#[derive(IntoElement)]
pub struct Sidebar {
    pub chrome: WeakEntity<Chrome>,
    pub width: f32,
    pub agents_ratio: f32,
    /// True while the title bar holds the tabs and has no room for New workspace.
    pub new_workspace_here: bool,
    pub pinned_only: bool,
    pub agent_sort: AgentSortMode,
    pub workspaces: Vec<SharedString>,
    pub active_workspace: usize,
}

fn section_label(id: &'static str, text: &'static str, cx: &App) -> impl IntoElement {
    let palette = theme::palette(cx);
    div()
        .id(id)
        .text_size(ui(TYPE_LABEL))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(palette.chrome_faint)
        .child(text.to_uppercase())
}

fn sort_toggle(
    id: &'static str,
    label: impl Into<SharedString>,
    text: impl Into<SharedString>,
    accent: gpui_kit::Rgba,
) -> Button {
    Button::new(id)
        .ghost()
        .xsmall()
        .compact()
        .h(ui(14.))
        .accessibility_label(label)
        .child(
            div()
                .text_size(ui(TYPE_LABEL))
                .text_color(accent)
                .child(text.into()),
        )
}

impl RenderOnce for Sidebar {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let palette = theme::palette(cx);
        let width = self.width;
        let chrome = self.chrome.clone();
        let rows = self.workspaces;
        let active = self.active_workspace;
        let section_head = |left: gpui_kit::AnyElement, right: gpui_kit::AnyElement| {
            h_flex()
                .flex_none()
                .items_center()
                .gap(ui(8.))
                .px(ui(8.))
                .pt(ui(4.))
                .pb(ui(6.))
                .child(left)
                .child(div().flex_1())
                .child(right)
        };

        let pinned_only = self.pinned_only;
        let toggle_chrome = self.chrome.clone();
        let filter = sort_toggle(
            "workspaces-filter",
            if pinned_only {
                "Showing pinned workspaces only. Show all workspaces."
            } else {
                "Showing all workspaces. Show pinned workspaces only."
            },
            if pinned_only { "pinned only" } else { "all" },
            palette.accent,
        )
        .on_click(move |_, _, cx| {
            toggle_chrome
                .update(cx, |chrome, cx| chrome.toggle_pinned_only(cx))
                .ok();
        });

        let new_workspace = self.new_workspace_here.then(|| {
            bar_button(
                "sidebar-new-workspace",
                IconName::Plus,
                "New workspace",
                disabled_ink(palette.chrome_faint),
            )
            .my(ui(-4.))
            .tooltip("New workspace")
            .disabled(true)
        });

        let sort = self.agent_sort;
        let sort_chrome = self.chrome.clone();
        let ordering = sort_toggle(
            "agents-sort",
            format!(
                "Agent ordering: {}. Next: {}.",
                layout::sort_mode_label(sort),
                layout::sort_mode_label(layout::next_sort_mode(sort))
            ),
            layout::sort_mode_label(sort),
            palette.accent,
        )
        .on_click(move |_, _, cx| {
            sort_chrome
                .update(cx, |chrome, cx| chrome.next_agent_sort(cx))
                .ok();
        });

        let quiet = |text: &'static str| {
            div()
                .px(ui(12.))
                .py(ui(10.))
                .text_size(ui(TYPE_SECONDARY))
                .text_color(palette.chrome_faint)
                .child(text)
        };

        let workspaces_list = div()
            .id("sidebar-workspaces-list")
            .role(Role::List)
            .aria_label("Workspaces")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .when(rows.is_empty(), |this| {
                this.child(quiet("No tmux sessions on this host yet."))
            })
            .children(rows.into_iter().enumerate().map(|(index, name)| {
                let selected = index == active;
                let chrome = chrome.clone();
                let label = name.clone();
                h_flex()
                    .id(("workspace-row", index))
                    .test_support()
                    .role(Role::Button)
                    .aria_label(label)
                    .aria_selected(selected)
                    .min_h(ui(28.))
                    .px(ui(8.))
                    .py(ui(4.))
                    .mb(ui(1.))
                    .gap(ui(6.))
                    .rounded(ui(RADIUS_ROW))
                    .text_size(ui(TYPE_ROW))
                    .line_height(relative(1.25))
                    .font_weight(FontWeight(WEIGHT_ROW))
                    .text_color(if selected {
                        palette.accent_ink
                    } else {
                        palette.chrome_ink
                    })
                    .when(selected, |this| this.bg(palette.accent))
                    .when(!selected, |this| {
                        this.hover(|style| style.bg(palette.chrome_hover))
                    })
                    .on_click(move |_, _, cx| {
                        chrome
                            .update(cx, |chrome, cx| chrome.select_workspace(index, cx))
                            .ok();
                    })
                    .when(index < 9, |this| {
                        this.child(
                            div()
                                .w(ui(12.))
                                .text_center()
                                .font_family(fonts::MONO)
                                .text_size(ui(TYPE_META))
                                .text_color(if selected {
                                    palette.accent_ink
                                } else {
                                    palette.chrome_faint
                                })
                                .child((index + 1).to_string()),
                        )
                    })
                    .child(div().min_w_0().truncate().child(name))
            }));

        let agents_list = div()
            .id("sidebar-agents-list")
            .role(Role::List)
            .aria_label("Agents")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .child(quiet(if self.pinned_only {
                "No agents in pinned workspaces."
            } else {
                "No agents detected."
            }));

        v_flex()
            .id("sidebar")
            .test_support()
            .role(Role::Navigation)
            .aria_label("Workspaces and agents")
            // CSS `line-height: normal`, under GPUI's roomier default.
            .line_height(relative(1.2))
            .relative()
            .flex_none()
            .h_full()
            .w(ui(width))
            .min_h_0()
            .px(ui(6.))
            .pt(ui(6.))
            .bg(palette.chrome_bg)
            .border_r_1()
            .border_color(palette.chrome_hairline)
            .child(
                v_flex()
                    .flex_1()
                    .min_h_0()
                    .child(section_head(
                        section_label("sidebar-workspaces-label", "Workspaces", cx)
                            .into_any_element(),
                        h_flex()
                            .gap(ui(4.))
                            .child(filter)
                            .children(new_workspace)
                            .into_any_element(),
                    ))
                    .child(workspaces_list),
            )
            .child(
                div()
                    .relative()
                    .flex_none()
                    .h(ui(9.))
                    .my(ui(2.))
                    .border_t_1()
                    .border_color(palette.chrome_hairline)
                    .child(divider(
                        self.chrome.clone(),
                        Divider::Agents,
                        HandleEdge::Leading,
                        false,
                        self.agents_ratio,
                        window,
                        cx,
                    ))
                    .child(
                        div()
                            .absolute()
                            .top(ui(2.))
                            .left(relative(0.5))
                            .ml(ui(-14.))
                            .w(ui(28.))
                            .h(ui(3.))
                            .rounded(ui(2.))
                            .bg(palette.chrome_border),
                    ),
            )
            .child(
                v_flex()
                    .flex_none()
                    .min_h_0()
                    .h(relative(self.agents_ratio))
                    .child(section_head(
                        section_label("sidebar-agents-label", "Agents", cx).into_any_element(),
                        ordering.into_any_element(),
                    ))
                    .child(agents_list),
            )
            .child(
                h_flex()
                    .id("host-row")
                    .test_support()
                    .role(Role::Button)
                    .aria_label("Host Local over local, disconnected. Open the host menu.")
                    .flex_none()
                    .items_center()
                    .gap(ui(7.))
                    .px(ui(8.))
                    .pt(ui(8.))
                    .pb(ui(7.))
                    .border_t_1()
                    .border_color(palette.chrome_hairline)
                    .font_family(fonts::MONO)
                    .text_size(ui(TYPE_META))
                    .text_color(palette.chrome_dim)
                    .child(
                        div()
                            .size(ui(7.))
                            .flex_none()
                            .rounded_full()
                            .bg(palette.danger),
                    )
                    .child(div().truncate().child("Local"))
                    .child(div().text_color(palette.chrome_faint).child("local")),
            )
            .child(divider(
                self.chrome,
                Divider::Sidebar,
                HandleEdge::Trailing,
                false,
                width,
                window,
                cx,
            ))
    }
}
