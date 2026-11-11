//! The one tab strip, a port of `features/workspaces/TabStrip.tsx`, in the bar
//! under the title bar or in the title bar itself: tmux windows and documents in
//! one row. Selecting a tab must not change any tab's width: nothing appears or
//! disappears on selection.
//!
//! A terminal tab has no close button: closing a tmux window ends processes,
//! and lives on the context menu and the Close command. A document closes in
//! place, because closing one throws nothing away.

use std::collections::HashMap;

use desktop_core::app_state::AppTabKind;
use gpui_kit::{
    AnyElement, App, ClickEvent, Entity, InteractiveElement, IntoElement, KeyDownEvent,
    MouseButton, MouseUpEvent, ParentElement, RenderOnce, ScrollHandle, SharedString,
    StatefulInteractiveElement, Styled, Window,
    assets::IconName,
    base::{Disableable, Tab, Tabs},
    canvas,
    component::{
        Icon, Sizable,
        button::{Button, ButtonVariants},
        h_flex,
        menu::{ContextMenuExt, DropdownMenu, PopupMenu, PopupMenuItem},
    },
    div,
    prelude::FluentBuilder,
    px,
};

use crate::{
    context_menu::{self, Entry, Item},
    fonts,
    theme::{
        self,
        metrics::{BAR_HEIGHT, TYPE_META, ui},
    },
    ui::{
        chrome::sidebar::{pin_glyph, state_color},
        title_bar::{bar_button, disabled_ink},
    },
    workspaces::{
        ShellView, Target, Workspaces, focus_terminal,
        model::{
            CombinedTab, TabKind, app_key, bulk_close_disabled, tabs_to_close_non_agent,
            tabs_to_close_others, tabs_to_close_right, takes_terminals,
        },
        moved_cursor, run_command,
    },
};

#[derive(IntoElement)]
pub struct TabStrip {
    pub workspaces: Entity<Workspaces>,
    pub in_title_bar: bool,
}

/// What the tab names to the commands, on the host the strip shows.
fn target(view: &ShellView, tab: &CombinedTab) -> Option<Target> {
    let scope = view.scope.clone()?;
    match &tab.kind {
        TabKind::Terminal { window_id, .. } => Some(Target::Window(scope, window_id.clone())),
        TabKind::App { tab_id, .. } => Some(Target::AppTab(scope, tab_id.clone())),
        TabKind::Pending => None,
    }
}

fn kind_label(tab: &CombinedTab) -> &'static str {
    match &tab.kind {
        TabKind::Terminal { .. } => "terminal",
        TabKind::Pending => "pending",
        TabKind::App { app_kind, .. } => match app_kind {
            AppTabKind::File => "file",
            AppTabKind::Markdown => "markdown",
            AppTabKind::GitDiff => "diff",
        },
    }
}

/// The all-tabs list's line: the kind only where another tab wears the same
/// title, and an agent's state where there is one.
fn all_tabs_label(tab: &CombinedTab, counts: &HashMap<&str, usize>) -> String {
    let kind = if counts.get(tab.title.as_str()).copied().unwrap_or(0) > 1 {
        format!(" ({})", kind_label(tab))
    } else {
        String::new()
    };
    let state = match &tab.kind {
        TabKind::Terminal {
            attention: Some(state),
            ..
        } => format!(" — agent {}", state.word()),
        _ => String::new(),
    };
    format!("{}{kind}{state}", tab.title)
}

/// Middle-click and the close button: a terminal goes through Close tab, a
/// document closes in place.
fn close(
    workspaces: &Entity<Workspaces>,
    target: Option<Target>,
    window: &mut Window,
    cx: &mut App,
) {
    if let Some(target) = target {
        run_command(workspaces, "window.close", Some(target), window, cx);
    }
}

fn small_button(id: String, label: String) -> Button {
    Button::new(SharedString::from(id))
        .ghost()
        .xsmall()
        .compact()
        .w(ui(18.))
        .accessibility_label(label)
}

impl RenderOnce for TabStrip {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let palette = theme::palette(cx);
        let state = self.workspaces.read(cx);
        let view = state.view(cx);
        let strip_focus = state.strip_focus.clone();
        let keyboard = strip_focus
            .is_focused(window)
            .then(|| state.strip_position(cx))
            .flatten();
        let tabs = view.tabs.clone();
        let active = view.active_key.clone();
        let docked = view.docked.as_ref().map(|(tab, _)| app_key(&tab.id));
        let can_mutate = view.can_mutate;
        let split = view.panes.len() > 1;
        let scroll = window
            .use_keyed_state("tab-strip-scroll", cx, |_, _| ScrollHandle::new())
            .read(cx)
            .clone();
        let clipped = scroll.max_offset().x > px(1.);
        // The active tab may be scrolled out; reveal it only when selection moves.
        let revealed = window.use_keyed_state("tab-strip-revealed", cx, |_, _| None::<String>);
        let active_index = tabs
            .iter()
            .position(|tab| Some(&tab.key) == active.as_ref());
        if *revealed.read(cx) != active {
            if let Some(index) = active_index {
                scroll.scroll_to_item(index);
            }
            revealed.update(cx, |revealed, _| *revealed = active.clone());
        }
        let count = tabs.len();
        let in_title_bar = self.in_title_bar;
        // Placeholders take no number: the numbers are what ⌃1–9 select.
        let mut shortcut = 0;
        let numbers: Vec<Option<usize>> = tabs
            .iter()
            .map(|tab| {
                (!tab.is_pending()).then(|| {
                    shortcut += 1;
                    shortcut
                })
            })
            .collect();

        let mut tab_elements: Vec<AnyElement> = Vec::new();
        for (index, tab) in tabs.iter().enumerate() {
            let is_active = active.as_deref() == Some(tab.key.as_str());
            let is_docked = docked.as_deref() == Some(tab.key.as_str());
            let underline = is_active.then_some(1.).or(is_docked.then_some(0.4));
            let id: SharedString = format!("workspace-tab-{}", tab.key).into();
            if tab.is_pending() {
                // A create the host has not answered: in the strip, so it
                // still reads as "tab N of M", but there is nothing to act on.
                tab_elements.push(
                    div()
                        .flex()
                        .flex_none()
                        .max_w(ui(220.))
                        .items_stretch()
                        .child(
                            Tab::new(id)
                                .disabled(true)
                                .accessibility_label(tab.title.clone())
                                .set_position(index + 1, count)
                                .flex()
                                .items_center()
                                .gap(ui(6.))
                                .px(ui(14.))
                                .text_size(ui(13.))
                                .text_color(palette.chrome_faint)
                                .child(div().size(ui(6.)).rounded_full().bg(palette.chrome_faint))
                                .child(div().italic().truncate().child(tab.title.clone())),
                        )
                        .into_any_element(),
                );
                continue;
            }
            let target = target(&view, tab);
            let terminal = matches!(tab.kind, TabKind::Terminal { .. });
            let group: SharedString = format!("tab-{}", tab.key).into();
            let mut label = Vec::new();
            let mut glyph: Option<AnyElement> = None;
            let mut trailing: Option<AnyElement> = None;
            let mut italic = false;
            let mut zoom = false;
            let mut dot = None;
            match &tab.kind {
                TabKind::Terminal {
                    window_id,
                    zoomed,
                    pinned,
                    attention,
                    ..
                } => {
                    if *pinned {
                        label.push("Pinned".to_owned());
                        glyph = Some(pin_glyph(palette.chrome_dim).into_any_element());
                    }
                    label.push(tab.title.clone());
                    if *zoomed {
                        label.push("Pane zoomed".into());
                        zoom = true;
                    }
                    if let Some(state) = attention {
                        label.push(format!("Agent {}", state.word()));
                        dot = Some(state_color(*state, cx));
                    }
                    let toggle = if *pinned {
                        format!("Unpin {}", tab.title)
                    } else {
                        format!("Pin {}", tab.title)
                    };
                    let (weak, window_id) = (self.workspaces.downgrade(), window_id.clone());
                    // Keeps its slot while hidden, so showing it cannot change
                    // the tab's width under the pointer.
                    trailing = Some(
                        div()
                            .flex()
                            .flex_none()
                            .items_center()
                            .mr(ui(4.))
                            .invisible()
                            .group_hover(group.clone(), |style| style.visible())
                            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                            .child(
                                small_button(format!("pin-{}", tab.key), toggle)
                                    .child(pin_glyph(palette.chrome_faint))
                                    .tooltip(if *pinned { "Unpin tab" } else { "Pin tab" })
                                    .on_click(move |_, _, cx| {
                                        weak.update(cx, |this, cx| {
                                            this.toggle_window_pin(&window_id, cx)
                                        })
                                        .ok();
                                    }),
                            )
                            .into_any_element(),
                    );
                }
                TabKind::App {
                    app_kind, preview, ..
                } => {
                    label.push(tab.title.clone());
                    italic = *preview;
                    let (icon, color) = match app_kind {
                        AppTabKind::GitDiff => (IconName::FileText, palette.state_working),
                        AppTabKind::Markdown => (IconName::FileText, palette.chrome_dim),
                        AppTabKind::File => (IconName::File, palette.chrome_dim),
                    };
                    glyph = Some(
                        Icon::new(icon)
                            .size(ui(12.))
                            .text_color(color)
                            .into_any_element(),
                    );
                    let (strip, target) = (self.workspaces.clone(), target.clone());
                    trailing = Some(
                        div()
                            .flex()
                            .flex_none()
                            .items_center()
                            .mr(ui(4.))
                            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                            .child(
                                small_button(
                                    format!("close-{}", tab.key),
                                    format!("Close {}", tab.title),
                                )
                                .icon(
                                    Icon::new(IconName::Close)
                                        .size(ui(11.))
                                        .text_color(palette.chrome_faint),
                                )
                                .on_click(move |_, window, cx| {
                                    close(&strip, target.clone(), window, cx)
                                }),
                            )
                            .into_any_element(),
                    );
                }
                TabKind::Pending => {}
            }
            let click = {
                let (weak, tab, target) =
                    (self.workspaces.downgrade(), tab.clone(), target.clone());
                move |event: &ClickEvent, window: &mut Window, cx: &mut App| {
                    let Some(this) = weak.upgrade() else {
                        return;
                    };
                    // Shift pins without selecting: pinning a background tab
                    // must not pull the terminal out from under the screen. A
                    // document has no window to pin, so Shift does nothing.
                    if event.modifiers().shift {
                        if let TabKind::Terminal { window_id, .. } = &tab.kind {
                            this.update(cx, |this, cx| this.toggle_window_pin(window_id, cx));
                        }
                        return;
                    }
                    if event.click_count() == 2 {
                        match &tab.kind {
                            // Double-clicking a preview makes it permanent.
                            TabKind::App { tab_id, .. } => {
                                let id = tab_id.clone();
                                this.update(cx, |this, cx| this.pin_document(&id, cx));
                            }
                            TabKind::Terminal { .. } if can_mutate => {
                                run_command(&this, "window.rename", target.clone(), window, cx);
                            }
                            _ => {}
                        }
                        return;
                    }
                    this.update(cx, |this, cx| {
                        this.strip_cursor = Some(index);
                        this.select_tab(&tab.key, cx);
                    });
                    if terminal {
                        focus_terminal(&this, window, cx);
                    }
                }
            };
            let middle = {
                let (strip, target) = (self.workspaces.clone(), target.clone());
                move |_: &MouseUpEvent, window: &mut Window, cx: &mut App| {
                    close(&strip, target.clone(), window, cx)
                }
            };
            let menu = tab_menu(&self.workspaces, &view, tab, &tabs, split && is_active);
            tab_elements.push(
                div()
                    .id(SharedString::from(format!("tab-cell-{}", tab.key)))
                    .group(group)
                    .relative()
                    .flex()
                    .flex_none()
                    .max_w(ui(220.))
                    .items_stretch()
                    .on_mouse_up(MouseButton::Middle, middle)
                    // Drawn over the cell, so the cursor walking the strip
                    // changes no tab's width.
                    .when(keyboard == Some(index), |this| {
                        this.child(
                            div()
                                .absolute()
                                .top(ui(1.))
                                .left(ui(1.))
                                .right(ui(1.))
                                // Clear of the active tab's underline.
                                .bottom(ui(3.))
                                .border_2()
                                .border_color(palette.accent)
                                .rounded(ui(4.)),
                        )
                    })
                    .child(
                        Tab::new(id)
                            .selected(is_active)
                            .accessibility_label(label.join(" "))
                            .set_position(index + 1, count)
                            .when(keyboard == Some(index), |tab| tab.aria_active_descendant())
                            .on_click(click)
                            .flex()
                            .min_w_0()
                            .items_center()
                            .gap(ui(6.))
                            .pl(ui(14.))
                            // The pin toggle beside it brings its own space.
                            .pr(ui(if terminal { 2. } else { 14. }))
                            .text_size(ui(13.))
                            .text_color(if is_active {
                                palette.chrome_ink_strong
                            } else if is_docked {
                                palette.chrome_ink
                            } else {
                                palette.chrome_dim
                            })
                            .when_some(numbers[index].filter(|n| *n <= 9), |tab, number| {
                                tab.child(
                                    div()
                                        .font_family(fonts::MONO)
                                        .text_size(ui(TYPE_META))
                                        .text_color(palette.chrome_faint)
                                        .child(number.to_string()),
                                )
                            })
                            .children(glyph)
                            .child(
                                div()
                                    .min_w_0()
                                    .truncate()
                                    .when(italic, |title| title.italic())
                                    .child(tab.title.clone()),
                            )
                            .when(zoom, |tab| {
                                tab.child(
                                    Icon::new(IconName::Maximize)
                                        .size(ui(11.))
                                        .text_color(palette.chrome_faint),
                                )
                            })
                            .when_some(dot, |tab, color| {
                                tab.child(div().flex_none().size(ui(8.)).rounded_full().bg(color))
                            }),
                    )
                    .children(trailing)
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
                    .context_menu(menu)
                    .into_any_element(),
            );
        }

        // The toolbar's anchor is the selected tab, the menu's the clicked one;
        // the same sets either way.
        let others = active
            .as_deref()
            .map_or_else(Vec::new, |key| tabs_to_close_others(&tabs, key));
        let non_agent = tabs_to_close_non_agent(&tabs);
        let tool = |id: &'static str, icon: IconName, label: &'static str, enabled: bool| {
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
            .tooltip(label)
            .disabled(!enabled)
        };
        let close_others = {
            let (weak, set, scope) = (
                self.workspaces.downgrade(),
                others.clone(),
                view.scope.clone(),
            );
            tool(
                "close-others",
                IconName::CircleX,
                "Close other tabs",
                !bulk_close_disabled(&others, can_mutate),
            )
            .on_click(move |_, _, cx| {
                weak.update(cx, |this, cx| {
                    this.close_tab_set(scope.clone(), set.clone(), false, cx)
                })
                .ok();
            })
        };
        let close_non_agent = {
            let (weak, set, scope) = (
                self.workspaces.downgrade(),
                non_agent.clone(),
                view.scope.clone(),
            );
            tool(
                "close-non-agent",
                IconName::Ban,
                "Close all non-agent tabs",
                !bulk_close_disabled(&non_agent, can_mutate),
            )
            .on_click(move |_, _, cx| {
                weak.update(cx, |this, cx| {
                    this.close_tab_set(scope.clone(), set.clone(), true, cx)
                })
                .ok();
            })
        };
        let new_terminal = {
            let weak = self.workspaces.downgrade();
            let session = view.session.as_ref().map(|session| session.id.clone());
            tool(
                "new-terminal",
                IconName::Plus,
                "New terminal tab",
                can_mutate && session.is_some(),
            )
            .on_click(move |_, _, cx| {
                if let Some(session) = &session {
                    weak.update(cx, |this, cx| this.create_window(session, cx))
                        .ok();
                }
            })
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

        let mut counts: HashMap<&str, usize> = HashMap::new();
        for tab in &tabs {
            *counts.entry(tab.title.as_str()).or_default() += 1;
        }
        let entries: Vec<(String, String, bool, bool)> = tabs
            .iter()
            .map(|tab| {
                (
                    tab.key.clone(),
                    all_tabs_label(tab, &counts),
                    active.as_deref() == Some(tab.key.as_str()),
                    tab.is_pending(),
                )
            })
            .collect();
        let all_tabs = {
            let weak = self.workspaces.downgrade();
            let scroll = scroll.clone();
            bar_button(
                "all-tabs",
                IconName::Ellipsis,
                "All tabs",
                palette.chrome_dim,
            )
            .dropdown_menu(move |mut menu, _, _| {
                for (index, (key, label, checked, pending)) in entries.iter().enumerate() {
                    let (weak, key, reveal) = (weak.clone(), key.clone(), scroll.clone());
                    menu = menu.item(
                        PopupMenuItem::new(label.clone())
                            .checked(*checked)
                            .disabled(*pending)
                            .on_click(move |_, window, cx| {
                                weak.update(cx, |this, cx| this.select_tab(&key, cx)).ok();
                                // Also for the tab already selected: the
                                // one this list was opened to find.
                                reveal.scroll_to_item(index);
                                if let (Some(this), true) =
                                    (weak.upgrade(), key.starts_with("terminal:"))
                                {
                                    focus_terminal(&this, window, cx);
                                }
                            }),
                    );
                }
                menu
            })
        };

        let (keys, follow) = (self.workspaces.downgrade(), scroll.clone());
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
                    .track_focus(&strip_focus)
                    .on_key_down(move |event: &KeyDownEvent, window, cx| {
                        let Some(this) = keys.upgrade() else {
                            return;
                        };
                        if event.keystroke.modifiers.modified() {
                            return;
                        }
                        let key = event.keystroke.key.as_str();
                        let cursor = this.read(cx).strip_position(cx);
                        if let Some(next) = moved_cursor(cursor, count, key, false) {
                            this.update(cx, |this, cx| {
                                this.strip_cursor = Some(next);
                                cx.notify();
                            });
                            follow.scroll_to_item(next);
                            cx.stop_propagation();
                        } else if matches!(key, "enter" | "space") {
                            let key = cursor.and_then(|index| {
                                this.read(cx)
                                    .view(cx)
                                    .tabs
                                    .get(index)
                                    .map(|tab| tab.key.clone())
                            });
                            if let Some(key) = key {
                                this.update(cx, |this, cx| this.select_tab(&key, cx));
                                if key.starts_with("terminal:") {
                                    focus_terminal(&this, window, cx);
                                }
                            }
                            cx.stop_propagation();
                        }
                    })
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
                    .child(close_others)
                    .child(close_non_agent)
                    .child(new_terminal)
                    .when(clipped, |this| this.child(all_tabs)),
            )
            .child(recheck)
    }
}

/// A tab's context menu. Opening it is not a selection: selecting first would
/// make a right-click on a terminal tab a real tmux select-window.
fn tab_menu(
    workspaces: &Entity<Workspaces>,
    view: &ShellView,
    tab: &CombinedTab,
    tabs: &[CombinedTab],
    closes_pane: bool,
) -> impl Fn(PopupMenu, &mut Window, &mut gpui_kit::Context<PopupMenu>) -> PopupMenu + 'static {
    let weak = workspaces.downgrade();
    let can_mutate = view.can_mutate;
    let scope = view.scope.clone();
    let tab = tab.clone();
    let target = target(view, &tab);
    let pane = view
        .active_pane
        .as_ref()
        .zip(view.scope.clone())
        .map(|(pane, scope)| Target::Pane(scope, pane.id.clone()));
    let docked = view
        .docked
        .as_ref()
        .is_some_and(|(docked, _)| tab.app_tab_id() == Some(docked.id.as_str()));
    let others = tabs_to_close_others(tabs, &tab.key);
    let right = tabs_to_close_right(tabs, &tab.key);
    let non_agent = tabs_to_close_non_agent(tabs);
    move |menu, _, _| {
        let command = |id: &'static str, target: Option<Target>| {
            let weak = weak.clone();
            move |window: &mut Window, cx: &mut App| {
                if let Some(this) = weak.upgrade() {
                    run_command(&this, id, target.clone(), window, cx);
                }
            }
        };
        let bulk = |set: &Vec<CombinedTab>, protect: bool| {
            let (weak, set, scope) = (weak.clone(), set.clone(), scope.clone());
            move |_: &mut Window, cx: &mut App| {
                weak.update(cx, |this, cx| {
                    this.close_tab_set(scope.clone(), set.clone(), protect, cx)
                })
                .ok();
            }
        };
        let terminal = matches!(tab.kind, TabKind::Terminal { .. });
        let mut entries: Vec<Entry> = Vec::new();
        if let TabKind::Terminal {
            window_id, pinned, ..
        } = &tab.kind
        {
            entries.push(
                Item::new("Rename tab…", command("window.rename", target.clone()))
                    .disabled(!can_mutate)
                    .into(),
            );
            let (weak, window_id) = (weak.clone(), window_id.clone());
            entries.push(
                Item::new(
                    if *pinned { "Unpin tab" } else { "Pin tab" },
                    move |_, cx| {
                        weak.update(cx, |this, cx| this.toggle_window_pin(&window_id, cx))
                            .ok();
                    },
                )
                .into(),
            );
        }
        entries.push(
            Item::new("Move left", command("window.moveLeft", target.clone()))
                .disabled(!tab.can_move_left || (terminal && !can_mutate))
                .into(),
        );
        entries.push(
            Item::new("Move right", command("window.moveRight", target.clone()))
                .disabled(!tab.can_move_right || (terminal && !can_mutate))
                .into(),
        );
        if let TabKind::App { tab_id, .. } = &tab.kind {
            let (weak, tab_id) = (weak.clone(), tab_id.clone());
            entries.push(
                Item::new(
                    if docked { "Undock" } else { "Move to the Side" },
                    move |_, cx| {
                        weak.update(cx, |this, cx| {
                            if docked {
                                this.undock(cx);
                            } else {
                                this.dock_tab(&tab_id, None, cx);
                            }
                        })
                        .ok();
                    },
                )
                .into(),
            );
        }
        entries.push(Entry::Separator);
        // No ellipsis on either: nothing here asks first.
        let close = if closes_pane {
            Item::new("Close Pane", command("pane.close", pane.clone()))
        } else {
            Item::new("Close tab", command("window.close", target.clone()))
        };
        let close = if terminal { close.destructive() } else { close };
        entries.push(close.disabled(terminal && !can_mutate).into());
        for (label, set, protect) in [
            ("Close Others", &others, false),
            ("Close Tabs to the Right", &right, false),
            ("Close All Non-Agent Tabs", &non_agent, true),
        ] {
            let item =
                Item::new(label, bulk(set, protect)).disabled(bulk_close_disabled(set, can_mutate));
            let item = if takes_terminals(set) {
                item.destructive()
            } else {
                item
            };
            entries.push(item.into());
        }
        context_menu::fill(menu, &entries)
    }
}
