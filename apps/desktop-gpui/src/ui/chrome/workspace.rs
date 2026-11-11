//! The work area between the rails: the tab strip when the title bar does not
//! hold it, and the terminal column with its optional docked document, a port of
//! `app/WorkspaceDock.tsx`'s divider and sizing. A document tab's surface comes
//! from [`crate::editor::surface`].

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
    terminal::integration::transfers::InternalPathDrag,
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
        let docked: Option<(SharedString, DockSide, AnyElement)> =
            view.docked.as_ref().map(|(tab, side)| {
                (
                    tab.title.clone().into(),
                    *side,
                    crate::editor::surface(tab, window, cx),
                )
            });
        let placeholder = || {
            div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .text_color(palette.chrome_faint)
                .child("Terminal")
                .into_any_element()
        };
        crate::editor::shown(
            view.selected_app_tab.as_ref(),
            view.docked.as_ref().map(|(tab, _)| tab),
            window,
            cx,
        );
        let body = match (&view.selected_app_tab, self.terminal) {
            (Some(tab), _) => crate::editor::surface(tab, window, cx),
            (None, Some(terminal)) => terminal.into_any_element(),
            (None, None) => placeholder(),
        };
        let mut body = Some(body);

        let tabs_strip = (!self.tabs_in_title_bar).then(|| TabStrip {
            workspaces: self.workspaces.clone(),
            in_title_bar: false,
        });

        // A document dragged over the workspace can dock at either edge.
        let dock_zones = dragdrop::payload(cx).is_some_and(|payload| payload.is_app_tab());
        // A path dragged from the Explorer or Git still pastes into the pane it
        // lands on, so its zones are a narrow strip at each edge. Drawn for any
        // in-app drag; only a path drop is taken.
        let path_zones = !dock_zones && cx.has_active_drag();
        let path_side = window.use_keyed_state("dock-path-side", cx, |_, _| None::<DockSide>);
        if !path_zones && path_side.read(cx).is_some() {
            path_side.update(cx, |held, _| *held = None);
        }
        let dock_hint = match dragdrop::marker(cx) {
            Some((_, DropTarget::Dock(side))) => Some(*side),
            _ => None,
        }
        .or_else(|| path_zones.then(|| *path_side.read(cx)).flatten());
        let chrome = self.chrome.clone();
        let dock_on_right = docked
            .as_ref()
            .is_none_or(|(_, side, _)| *side == DockSide::Right);
        let share = relative(self.dock_fraction);
        // A saved docked tab with no record is the same as no dock at all.
        let no_dock = docked.is_none();

        v_flex()
            .id("workspace")
            .test_support()
            .role(Role::Main)
            .aria_label(match &view.session {
                Some(session) => format!("Workspace {}", session.name),
                None => "Workspace".to_owned(),
            })
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
                    .when_some(docked, |this, (title, side, surface)| {
                        let doc = div()
                            .id("docked-document")
                            .key_context(crate::editor::DOCKED_CONTEXT)
                            .on_mouse_move(|event: &MouseMoveEvent, window, cx| {
                                let pressed = event.pressed_button.is_some();
                                crate::editor::dock_hover_moved(pressed, window, cx);
                            })
                            .on_hover(|hovered, _, cx| {
                                if !*hovered {
                                    crate::editor::dock_hover_left(cx);
                                }
                            })
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
                            .child(surface)
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
                                .child(div().flex_1().min_w_0().h_full().children(body.take()))
                                .child(doc),
                            DockSide::Left => this
                                .child(doc)
                                .child(div().flex_1().min_w_0().h_full().children(body.take())),
                        }
                    })
                    .when(no_dock, |this| this.children(body.take()))
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
                    .when(path_zones, |this| {
                        let zone = |side: DockSide| {
                            let path_side = path_side.clone();
                            let strip = div()
                                .id(match side {
                                    DockSide::Left => "dock-path-left",
                                    DockSide::Right => "dock-path-right",
                                })
                                .absolute()
                                .top_0()
                                .h_full()
                                .w(relative(0.12))
                                .max_w(ui(120.))
                                // Every strip hears every move: the one under the
                                // pointer claims the hint, the other lets go of it.
                                .on_drag_move::<InternalPathDrag>(move |event, _, cx| {
                                    let over = event.bounds.contains(&event.event.position);
                                    path_side.update(cx, |held, cx| {
                                        let next = match (over, *held) {
                                            (true, _) => Some(side),
                                            (false, Some(current)) if current == side => None,
                                            (false, current) => current,
                                        };
                                        if *held != next {
                                            *held = next;
                                            cx.notify();
                                        }
                                    });
                                })
                                .on_drop(move |drag: &InternalPathDrag, _, cx| {
                                    crate::files::dock_path(drag, side, cx)
                                });
                            match side {
                                DockSide::Left => strip.left_0(),
                                DockSide::Right => strip.right_0(),
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
