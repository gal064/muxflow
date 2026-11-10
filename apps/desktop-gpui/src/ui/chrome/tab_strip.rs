//! The one tab strip, a port of `features/workspaces/TabStrip.tsx`, in the bar
//! under the title bar or in the title bar itself. Selecting a tab must not
//! change any tab's width: nothing appears or disappears on selection.

use gpui_kit::{
    App, InteractiveElement, IntoElement, MouseButton, ParentElement, RenderOnce, ScrollHandle,
    SharedString, StatefulInteractiveElement, Styled, WeakEntity, Window,
    assets::IconName,
    base::{Disableable, Tab, Tabs},
    canvas,
    component::{
        Icon, Sizable,
        button::{Button, ButtonVariants},
        h_flex,
        menu::{DropdownMenu, PopupMenuItem},
    },
    div,
    prelude::FluentBuilder,
    px,
};

use crate::{
    fonts,
    theme::{
        self,
        metrics::{BAR_HEIGHT, TYPE_META, ui},
    },
    ui::{
        chrome::Chrome,
        title_bar::{bar_button, disabled_ink},
    },
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TabEntry {
    pub key: SharedString,
    pub title: SharedString,
    pub preview: bool,
}

#[derive(IntoElement)]
pub struct TabStrip {
    pub chrome: WeakEntity<Chrome>,
    pub tabs: Vec<TabEntry>,
    pub active: Option<SharedString>,
    pub docked: Option<SharedString>,
    pub in_title_bar: bool,
}

impl RenderOnce for TabStrip {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let palette = theme::palette(cx);
        let scroll = window
            .use_keyed_state("tab-strip-scroll", cx, |_, _| ScrollHandle::new())
            .read(cx)
            .clone();
        let clipped = scroll.max_offset().x > px(1.);
        // The active tab may be scrolled out; reveal it only when selection moves.
        let revealed =
            window.use_keyed_state("tab-strip-revealed", cx, |_, _| None::<SharedString>);
        let active_index = self
            .tabs
            .iter()
            .position(|tab| Some(&tab.key) == self.active.as_ref());
        if *revealed.read(cx) != self.active {
            if let Some(index) = active_index {
                scroll.scroll_to_item(index);
            }
            revealed.update(cx, |revealed, _| *revealed = self.active.clone());
        }
        let count = self.tabs.len();
        let key_of = |key: &SharedString| format!("workspace-tab-{key}");
        let in_title_bar = self.in_title_bar;

        let tab_elements = self.tabs.iter().enumerate().map(|(index, entry)| {
            let active = self.active.as_deref() == Some(entry.key.as_str());
            let docked = self.docked.as_deref() == Some(entry.key.as_str());
            let underline = active.then_some(1.).or(docked.then_some(0.4));
            let select = {
                let chrome = self.chrome.clone();
                let key = entry.key.clone();
                move |_: &gpui_kit::ClickEvent, _: &mut Window, cx: &mut App| {
                    chrome
                        .update(cx, |chrome, cx| chrome.select_tab(key.clone(), cx))
                        .ok();
                }
            };
            let close = {
                let chrome = self.chrome.clone();
                let key = entry.key.clone();
                move |cx: &mut App| {
                    chrome
                        .update(cx, |chrome, cx| chrome.close_tab(&key, cx))
                        .ok();
                }
            };
            let middle_close = close.clone();
            div()
                .relative()
                .flex()
                .flex_none()
                .max_w(ui(220.))
                .items_stretch()
                .on_mouse_up(MouseButton::Middle, move |_, _, cx| middle_close(cx))
                .child(
                    Tab::new(key_of(&entry.key))
                        .selected(active)
                        .accessibility_label(entry.title.clone())
                        .set_position(index + 1, count)
                        .on_click(select)
                        .flex()
                        .min_w_0()
                        .items_center()
                        .gap(ui(6.))
                        .px(ui(14.))
                        .text_size(ui(13.))
                        .text_color(if active {
                            palette.chrome_ink_strong
                        } else if docked {
                            palette.chrome_ink
                        } else {
                            palette.chrome_dim
                        })
                        .when(index < 9, |tab| {
                            tab.child(
                                div()
                                    .font_family(fonts::MONO)
                                    .text_size(ui(TYPE_META))
                                    .text_color(palette.chrome_faint)
                                    .child((index + 1).to_string()),
                            )
                        })
                        .child(
                            div()
                                .min_w_0()
                                .truncate()
                                .when(entry.preview, |title| title.italic())
                                .child(entry.title.clone()),
                        ),
                )
                .child(
                    div()
                        .flex()
                        .flex_none()
                        .items_center()
                        .mr(ui(4.))
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .child(
                            Button::new(SharedString::from(format!("close-{}", entry.key)))
                                .ghost()
                                .xsmall()
                                .compact()
                                .w(ui(18.))
                                .icon(
                                    Icon::new(IconName::Close)
                                        .size(ui(11.))
                                        .text_color(palette.chrome_faint),
                                )
                                .accessibility_label(format!("Close {}", entry.title))
                                .on_click(move |_, _, cx| close(cx)),
                        ),
                )
                .when_some(underline, |this, opacity| {
                    this.child(
                        div()
                            .absolute()
                            .left(ui(8.))
                            .right(ui(8.))
                            .bottom_0()
                            .h(ui(2.))
                            .rounded_t(ui(1.))
                            .bg(palette.accent)
                            .opacity(opacity),
                    )
                })
        });

        let tool = |id: &'static str, icon: IconName, label: &'static str| {
            bar_button(id, icon, label, disabled_ink(palette.chrome_faint)).disabled(true)
        };

        // `clipped` comes from the last layout; this runs after the new one, so
        // a resize does not leave the "All tabs" button a frame behind.
        let recheck = {
            let scroll = scroll.clone();
            canvas(
                move |_, window, cx| {
                    if (scroll.max_offset().x > px(1.)) != clipped {
                        window.defer(cx, |window, _| window.refresh());
                    }
                },
                |_, _, _, _| {},
            )
            .absolute()
        };

        h_flex()
            .id("tab-strip")
            .when(!in_title_bar, |this| {
                this.h(ui(BAR_HEIGHT))
                    .bg(palette.term_bg)
                    .border_b_1()
                    .border_color(palette.chrome_hairline)
            })
            .when(in_title_bar, |this| this.h_full())
            .w_full()
            .min_w_0()
            .items_stretch()
            .px(ui(4.))
            .gap(ui(1.))
            .child(
                Tabs::new("workspace-tabs")
                    .aria_label("Terminal tabs and documents")
                    .flex()
                    .min_w_0()
                    .overflow_x_scroll()
                    .track_scroll(&scroll)
                    .children(tab_elements),
            )
            .child(
                h_flex()
                    .id("tab-strip-tools")
                    .flex_none()
                    .items_center()
                    .gap(ui(2.))
                    .ml_auto()
                    .pl(ui(6.))
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .child(tool("close-others", IconName::CircleX, "Close other tabs"))
                    .child(tool(
                        "close-non-agent",
                        IconName::Ban,
                        "Close all non-agent tabs",
                    ))
                    .child(tool("new-terminal", IconName::Plus, "New terminal tab"))
                    .when(clipped, |this| {
                        let tabs = self.tabs.clone();
                        let active = self.active.clone();
                        let chrome = self.chrome.clone();
                        this.child(
                            bar_button(
                                "all-tabs",
                                IconName::Ellipsis,
                                "All tabs",
                                palette.chrome_dim,
                            )
                            .dropdown_menu(move |mut menu, _, _| {
                                for tab in &tabs {
                                    let chrome = chrome.clone();
                                    let key = tab.key.clone();
                                    let reveal = scroll.clone();
                                    let index = tabs.iter().position(|t| t.key == tab.key);
                                    let checked = active.as_ref() == Some(&tab.key);
                                    menu = menu.item(
                                        PopupMenuItem::new(tab.title.clone())
                                            .checked(checked)
                                            .on_click(move |_, _, cx| {
                                                chrome
                                                    .update(cx, |chrome, cx| {
                                                        chrome.select_tab(key.clone(), cx)
                                                    })
                                                    .ok();
                                                // Also for the tab already selected:
                                                // the one this list was opened to find.
                                                if let Some(index) = index {
                                                    reveal.scroll_to_item(index);
                                                }
                                            }),
                                    );
                                }
                                menu
                            }),
                        )
                    }),
            )
            .child(recheck)
    }
}
