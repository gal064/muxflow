//! The work area between the rails: the tab strip when the title bar does not
//! hold it, and the terminal column with its optional docked document, a port of
//! `app/WorkspaceDock.tsx`'s divider and sizing. Content is a placeholder until
//! terminals and document panels arrive.

use desktop_core::app_state::DockSide;
use gpui_kit::{
    AnyElement, AnyView, App, InteractiveElement, IntoElement, ParentElement, RenderOnce, Role,
    SharedString, StatefulInteractiveElement, Styled, WeakEntity, Window,
    base::{HandleEdge, TestSupportExt},
    component::{h_flex, v_flex},
    div,
    prelude::FluentBuilder,
    relative,
};

use crate::{
    theme,
    ui::chrome::{
        Chrome, WorkspacePlaceholder,
        divider::{Divider, DividerDrag, divider},
        tab_strip::TabStrip,
    },
};

#[derive(IntoElement)]
pub struct WorkspaceArea {
    pub chrome: WeakEntity<Chrome>,
    pub workspace: Option<WorkspacePlaceholder>,
    pub dock_fraction: f32,
    pub tabs_in_title_bar: bool,
    /// The terminal surface, when a real connection is running.
    pub terminal: Option<AnyView>,
}

impl RenderOnce for WorkspaceArea {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let palette = theme::palette(cx);
        let workspace = self.workspace;
        let empty_label: SharedString = "No workspace".into();
        let (body_label, docked): (SharedString, Option<(SharedString, DockSide)>) = workspace
            .as_ref()
            .map_or((empty_label.clone(), None), |workspace| {
                let title_of = |key: &SharedString| {
                    workspace
                        .tabs
                        .iter()
                        .find(|tab| &tab.key == key)
                        .map(|tab| tab.title.clone())
                };
                let body = workspace
                    .active_tab
                    .as_ref()
                    .and_then(title_of)
                    .unwrap_or_else(|| "Terminal".into());
                let docked = workspace
                    .docked
                    .as_ref()
                    .and_then(|(key, side)| title_of(key).map(|title| (title, *side)));
                (body, docked)
            });

        let placeholder = |label: SharedString| {
            div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .text_color(palette.chrome_faint)
                .child(label)
        };
        let terminal = self
            .terminal
            .filter(|_| workspace.as_ref().is_none_or(|w| w.active_tab.is_none()));
        let body = |label: SharedString| -> AnyElement {
            match terminal.clone() {
                Some(view) => view.into_any_element(),
                None => placeholder(label).into_any_element(),
            }
        };

        let tabs_strip = (!self.tabs_in_title_bar).then(|| TabStrip {
            chrome: self.chrome.clone(),
            tabs: workspace
                .as_ref()
                .map_or_else(Vec::new, |workspace| workspace.tabs.clone()),
            active: workspace.as_ref().and_then(|w| w.active_tab.clone()),
            docked: workspace
                .as_ref()
                .and_then(|w| w.docked.as_ref().map(|(key, _)| key.clone())),
            in_title_bar: false,
        });

        let chrome = self.chrome.clone();
        let dock_on_right = docked
            .as_ref()
            .is_none_or(|(_, side)| *side == DockSide::Right);
        let share = relative(self.dock_fraction);
        // A saved docked tab with no record is the same as no dock at all.
        let no_dock = docked.is_none();

        v_flex()
            .id("workspace")
            .test_support()
            .role(Role::Main)
            .aria_label("Workspace")
            .flex_1()
            .min_w_0()
            .min_h_0()
            .h_full()
            .bg(palette.term_bg)
            .children(tabs_strip)
            .child(
                h_flex()
                    .id("workspace-content")
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .min_w_0()
                    .overflow_hidden()
                    .on_drag_move::<DividerDrag>(move |event, window, cx| {
                        let DividerDrag(dragged) = *event.drag(cx);
                        if dragged != Divider::Dock {
                            return;
                        }
                        chrome
                            .update(cx, |chrome, cx| {
                                chrome.drag_to(
                                    dragged,
                                    event.event.position,
                                    event.bounds,
                                    window,
                                    cx,
                                )
                            })
                            .ok();
                    })
                    .when_some(docked, |this, (title, side)| {
                        let doc = div()
                            .id("docked-document")
                            .role(Role::Group)
                            .aria_label(title.clone())
                            .relative()
                            .flex_none()
                            .h_full()
                            .w(share)
                            .bg(palette.term_bg)
                            .border_color(palette.chrome_hairline)
                            .map(|doc| match side {
                                DockSide::Right => doc.border_l_1(),
                                DockSide::Left => doc.border_r_1(),
                            })
                            .child(placeholder(title))
                            .child(divider(
                                self.chrome.clone(),
                                Divider::Dock,
                                match side {
                                    DockSide::Right => HandleEdge::Leading,
                                    DockSide::Left => HandleEdge::Trailing,
                                },
                                dock_on_right,
                                self.dock_fraction,
                                window,
                                cx,
                            ));
                        match side {
                            DockSide::Right => this
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .h_full()
                                        .child(body(body_label.clone())),
                                )
                                .child(doc),
                            DockSide::Left => this.child(doc).child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .h_full()
                                    .child(body(body_label.clone())),
                            ),
                        }
                    })
                    .when(no_dock, |this| this.child(body(body_label.clone()))),
            )
    }
}
