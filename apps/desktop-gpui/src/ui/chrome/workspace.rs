//! The work area between the rails: the tab strip when the title bar does not
//! hold it, and the terminal column with its optional docked document, a port of
//! `app/WorkspaceDock.tsx`'s divider and sizing. A document shows a placeholder
//! until the document panels arrive.

use desktop_core::app_state::DockSide;
use gpui_kit::{
    AnyElement, AnyView, App, Entity, InteractiveElement, IntoElement, MouseMoveEvent,
    ParentElement, RenderOnce, Role, SharedString, StatefulInteractiveElement, Styled, WeakEntity,
    Window,
    base::{HandleEdge, TestSupportExt},
    component::{h_flex, v_flex},
    div,
    prelude::FluentBuilder,
    relative,
};

use crate::{
    dragdrop::{self, DropTarget},
    theme::{self, metrics::ui},
    ui::chrome::{
        Chrome,
        divider::{Divider, DividerDrag, divider},
        tab_strip::TabStrip,
    },
    workspaces::Workspaces,
};

#[derive(IntoElement)]
pub struct WorkspaceArea {
    pub chrome: WeakEntity<Chrome>,
    pub workspaces: Entity<Workspaces>,
    pub dock_fraction: f32,
    pub tabs_in_title_bar: bool,
    /// The terminal surface, when a real connection is running.
    pub terminal: Option<AnyView>,
}

impl RenderOnce for WorkspaceArea {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let palette = theme::palette(cx);
        let view = self.workspaces.read(cx).view(cx);
        // The documents themselves arrive with the file features; until then a
        // document tab shows its title where its surface will be.
        let body_label: SharedString = view
            .selected_app_tab
            .as_ref()
            .map_or_else(|| "Terminal".into(), |tab| tab.title.clone().into());
        let docked: Option<(SharedString, DockSide)> = view
            .docked
            .as_ref()
            .map(|(tab, side)| (tab.title.clone().into(), *side));

        let placeholder = |label: SharedString| {
            div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .text_color(palette.chrome_faint)
                .child(label)
        };
        let terminal = self.terminal.filter(|_| view.selected_app_tab.is_none());
        let body = |label: SharedString| -> AnyElement {
            match terminal.clone() {
                Some(view) => view.into_any_element(),
                None => placeholder(label).into_any_element(),
            }
        };

        let tabs_strip = (!self.tabs_in_title_bar).then(|| TabStrip {
            workspaces: self.workspaces.clone(),
            in_title_bar: false,
        });

        // A document dragged over the workspace can dock at either edge.
        let dock_zones = dragdrop::payload(cx).is_some_and(|payload| payload.is_app_tab());
        let dock_hint = match dragdrop::marker(cx) {
            Some((_, DropTarget::Dock(side))) => Some(*side),
            _ => None,
        };
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
                    .when(no_dock, |this| this.child(body(body_label.clone())))
                    .when(dock_zones, |this| {
                        let zone = |side: DockSide| {
                            let zone = div()
                                .id(match side {
                                    DockSide::Left => "dock-zone-left",
                                    DockSide::Right => "dock-zone-right",
                                })
                                .absolute()
                                .top_0()
                                .h_full()
                                .w(relative(0.25))
                                .on_mouse_move(move |_: &MouseMoveEvent, _, cx| {
                                    dragdrop::offer(DropTarget::Dock(side), cx);
                                });
                            match side {
                                DockSide::Left => zone.left_0(),
                                DockSide::Right => zone.right_0(),
                            }
                        };
                        this.child(zone(DockSide::Left))
                            .child(zone(DockSide::Right))
                    })
                    // Where the document would dock, at the share it would take.
                    .when_some(dock_hint, |this, side| {
                        let hint = div()
                            .absolute()
                            .top(ui(6.))
                            .bottom(ui(6.))
                            .w(share)
                            .border_2()
                            .border_dashed()
                            .border_color(palette.accent)
                            .rounded(ui(6.))
                            .bg(palette.accent_wash);
                        this.child(match side {
                            DockSide::Left => hint.left(ui(6.)),
                            DockSide::Right => hint.right(ui(6.)),
                        })
                    }),
            )
    }
}
