//! Context menus, on Kit's `PopupMenu`. Port of the Tauri app's `ui/ContextMenu.tsx`.
//!
//! Attach a menu to an element with Kit's `.context_menu(...)`, and fill it with
//! [`fill`]. Kit supplies the arrow keys, Escape, dismissal on an outside press,
//! and keeping the menu inside the window. This module adds what Kit lacks:
//!
//! - Opening from the keyboard. A surface that holds focus calls [`open`] on
//!   [`is_menu_key`], and draws [`anchor`] in the row the menu is for, so the
//!   menu sits under that row as Tauri's `anchorForElement` placed it.
//! - Home and End, Tab and Shift+Tab stepping through the items as the arrows
//!   do, and opening on the first item, or on the checked one.
//! - An item runs after the menu has closed and given focus back to its opener,
//!   so a dialog the item opens hands focus back there when it closes, as in
//!   Tauri.
//!
//! A shortcut shows when an item names its action, and follows the overrides
//! ([`crate::menus`] keeps the bindings it reads).

use std::{cell::RefCell, rc::Rc};

use gpui_kit::{
    Action, AnyElement, AnyWindowHandle, App, Bounds, Context, DismissEvent, Entity, FocusHandle,
    Focusable, Global, IntoElement, Keystroke, KeystrokeEvent, ParentElement, Pixels, Point,
    ScrollHandle, SharedString, Styled, Subscription, WeakEntity, WeakFocusHandle, Window,
    anchored,
    base::{
        POPUP_PRIORITY,
        actions::{SelectDown, SelectUp},
    },
    canvas,
    component::menu::{PopupMenu, PopupMenuItem},
    deferred, div, point, px,
};

use crate::theme;

type Run = Rc<dyn Fn(&mut Window, &mut App)>;

/// A row's menu as its right-click builds it, kept so the keyboard can open
/// the same menu.
pub type Builder = Rc<dyn Fn(PopupMenu, &mut Window, &mut Context<PopupMenu>) -> PopupMenu>;

#[derive(Clone)]
pub struct Item {
    label: SharedString,
    disabled: bool,
    destructive: bool,
    checked: Option<bool>,
    hint: Option<Rc<dyn Action>>,
    run: Run,
}

impl Item {
    pub fn new(
        label: impl Into<SharedString>,
        run: impl Fn(&mut Window, &mut App) + 'static,
    ) -> Self {
        Self {
            label: label.into(),
            disabled: false,
            destructive: false,
            checked: None,
            hint: None,
            run: Rc::new(run),
        }
    }

    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    /// The app's one destructive treatment: there is only one red.
    pub fn destructive(mut self) -> Self {
        self.destructive = true;
        self
    }

    /// Makes the item one option of a set, so a menu that picks something can
    /// say which one is picked.
    pub fn checked(mut self, checked: bool) -> Self {
        self.checked = Some(checked);
        self
    }

    /// Shows this action's shortcut beside the label.
    pub fn hint(mut self, action: &dyn Action) -> Self {
        self.hint = Some(action.boxed_clone().into());
        self
    }

    fn popup(&self) -> PopupMenuItem {
        let label = self.label.clone();
        // Disabled, it is dimmed like any other item rather than kept red.
        let item = if self.destructive && !self.disabled {
            PopupMenuItem::element(move |_, cx| {
                div()
                    .text_color(theme::palette(cx).danger_ink)
                    .child(label.clone())
                    .into_any_element()
            })
        } else {
            PopupMenuItem::new(label)
        };
        let run = self.run.clone();
        let item = item
            .disabled(self.disabled)
            .on_click(move |_, window, cx| {
                // After Kit closes the menu and gives focus back to its opener,
                // so a dialog the item opens returns focus there when it closes,
                // as in Tauri, not to the closed menu. Twice:
                // a dropdown's popover gives focus back on the menu's dismiss
                // event, which is queued after this click.
                let run = run.clone();
                window.defer(cx, move |window, cx| {
                    window.defer(cx, move |window, cx| run(window, cx));
                });
            })
            .checked(self.checked.unwrap_or(false));
        match &self.hint {
            // Only the hint: a handler runs the item.
            Some(action) => item.action(action.boxed_clone()),
            None => item,
        }
    }
}

#[derive(Clone)]
pub enum Entry {
    Item(Item),
    Separator,
    /// A second level of the menu, opening beside it.
    Submenu {
        label: SharedString,
        disabled: bool,
        entries: Rc<[Entry]>,
    },
}

impl Entry {
    pub fn submenu(label: impl Into<SharedString>, entries: Vec<Entry>, disabled: bool) -> Self {
        Self::Submenu {
            label: label.into(),
            disabled,
            entries: entries.into(),
        }
    }
}

impl From<Item> for Entry {
    fn from(item: Item) -> Self {
        Self::Item(item)
    }
}

/// The last menu opened, until it closes. Kit focuses an open menu, so the
/// menu is open while it, or a submenu inside it, holds focus. Closing gives
/// focus back only when there was focus to give back, so the close itself
/// also ends it.
struct OpenMenu(FocusHandle);

impl Global for OpenMenu {}

/// Whether a context menu is open: a tooltip asked for now would be drawn
/// over it.
pub fn is_open(window: &Window, cx: &App) -> bool {
    cx.try_global::<OpenMenu>()
        .is_some_and(|open| open.0.contains_focused(window, cx))
}

/// The menus [`fill`] built that are still alive, with what they hold, so a
/// menu can be rebuilt to put its selection back to nothing: Kit 0.9 has no
/// way to set the selection, and Home, End and opening on an item need one.
type BuiltMenu = (WeakEntity<PopupMenu>, Rc<[Entry]>);

#[derive(Default)]
struct Built(Vec<BuiltMenu>);

impl Global for Built {}

/// Where each surface's keyboard row was last drawn, by the focus that holds
/// the surface.
#[derive(Default)]
struct Anchor(RefCell<Vec<(WeakFocusHandle, Bounds<Pixels>)>>);

impl Global for Anchor {}

/// A menu opened from the keyboard: Kit's own opens only under a pointer.
struct KeyboardMenu {
    window: AnyWindowHandle,
    menu: Entity<PopupMenu>,
    at: Point<Pixels>,
    _dismiss: Subscription,
}

impl Global for KeyboardMenu {}

/// Called once from [`crate::features::init`].
pub fn init(cx: &mut App) {
    cx.set_global(Built::default());
    cx.set_global(Anchor::default());
    cx.intercept_keystrokes(menu_keys).detach();
}

/// The keyboard's way of saying "right-click": Shift+F10, which a Mac keyboard
/// reaches through Fn+F10, or the Menu key most PC keyboards have.
pub fn is_menu_key(keystroke: &Keystroke) -> bool {
    keystroke.key == "menu" || (keystroke.key == "f10" && keystroke.modifiers.shift)
}

/// Drawn inside the row the keyboard is on in the surface `focus` holds, so a
/// menu [`open`]ed from there sits under that row. The row must be
/// `relative()`.
pub fn anchor(focus: &FocusHandle) -> impl IntoElement {
    let focus = focus.downgrade();
    canvas(
        move |bounds, _, cx| {
            if let Some(anchor) = cx.try_global::<Anchor>() {
                let mut rows = anchor.0.borrow_mut();
                rows.retain(|(row, _)| *row != focus && row.upgrade().is_some());
                rows.push((focus, bounds));
            }
        },
        |_, _, _, _| {},
    )
    .absolute()
    .size_full()
}

/// Keeps the row `focus` holds in view of `scroll` each time it takes the
/// keyboard, by Tab or an arrow, as a browser scrolls to what it focuses. Called
/// while drawing the row, which draws [`anchor`]; `id` is the row's own.
pub fn reveal_when_focused(
    id: impl Into<SharedString>,
    focus: &FocusHandle,
    scroll: &ScrollHandle,
    window: &mut Window,
    cx: &mut App,
) {
    let (focus, scroll) = (focus.clone(), scroll.clone());
    window.use_keyed_state(id.into(), cx, move |window, cx| {
        let row = focus.downgrade();
        window.on_focus_in(&focus, cx, move |window, _| {
            // A press focuses the row too, and a browser leaves that be.
            if let Some(row) = row.upgrade().filter(|_| window.last_input_was_keyboard()) {
                reveal(&row, &scroll, window);
            }
        })
    });
}

/// Scrolls `scroll` so the row `focus` holds is in view once it is drawn.
fn reveal(focus: &FocusHandle, scroll: &ScrollHandle, window: &mut Window) {
    let (focus, scroll) = (focus.downgrade(), scroll.clone());
    // The first frame draws the row where it is; the second reads it.
    window.on_next_frame(move |window, _| {
        window.on_next_frame(move |window, cx| {
            let row = cx.try_global::<Anchor>().and_then(|anchor| {
                let rows = anchor.0.borrow();
                rows.iter()
                    .find(|(row, _)| *row == focus)
                    .map(|(_, bounds)| *bounds)
            });
            let (Some(row), view) = (row, scroll.bounds()) else {
                return;
            };
            let mut offset = scroll.offset();
            if row.top() < view.top() {
                offset.y += view.top() - row.top();
            } else if row.bottom() > view.bottom() {
                offset.y -= row.bottom() - view.bottom();
            } else {
                return;
            }
            scroll.set_offset(offset);
            window.refresh();
        })
    });
}

/// Opens a menu from the keyboard, under the row the focused surface marked
/// with [`anchor`]. `build` is what the row's right-click menu runs.
pub fn open(
    build: impl FnOnce(PopupMenu, &mut Window, &mut Context<PopupMenu>) -> PopupMenu,
    window: &mut Window,
    cx: &mut App,
) {
    // The key inside a pointer's menu reaches the row under it: one menu.
    if is_open(window, cx) {
        return;
    }
    let opener = window.focused(cx);
    let marked = cx
        .try_global::<Anchor>()
        .zip(opener.as_ref())
        .and_then(|(anchor, opener)| {
            let rows = anchor.0.borrow();
            rows.iter()
                .find(|(row, _)| row == opener)
                .map(|(_, bounds)| point(bounds.left() + px(8.), bounds.bottom()))
        });
    let at = marked.unwrap_or_else(|| window.mouse_position());
    let menu = PopupMenu::build(window, cx, |menu, window, cx| {
        // Kit gives focus back to the action context as the menu closes.
        let menu = match &opener {
            Some(opener) => menu.action_context(opener.clone()),
            None => menu,
        };
        build(menu, window, cx)
    });
    menu.focus_handle(cx).focus(window, cx);
    let dismiss = window.subscribe(&menu, cx, |_, _: &DismissEvent, window, cx| {
        cx.remove_global::<KeyboardMenu>();
        window.refresh();
    });
    cx.set_global(KeyboardMenu {
        window: window.window_handle(),
        menu,
        at,
        _dismiss: dismiss,
    });
    window.refresh();
}

/// The menu [`open`] showed in this window, drawn over everything. The window
/// root draws it.
pub fn keyboard_menu(window: &Window, cx: &App) -> Option<AnyElement> {
    let open = cx
        .try_global::<KeyboardMenu>()
        .filter(|open| open.window == window.window_handle())?;
    Some(
        deferred(
            anchored()
                .position(open.at)
                .snap_to_window_with_margin(px(8.))
                .child(open.menu.clone()),
        )
        .with_priority(POPUP_PRIORITY)
        .into_any_element(),
    )
}

/// Fills `menu` with `entries`.
pub fn fill(
    menu: PopupMenu,
    entries: &[Entry],
    window: &mut Window,
    cx: &mut Context<PopupMenu>,
) -> PopupMenu {
    let focus = menu.focus_handle(cx);
    cx.set_global(OpenMenu(focus.clone()));
    cx.subscribe_self(|menu: &mut PopupMenu, _: &DismissEvent, cx| {
        let focus = menu.focus_handle(cx);
        if cx
            .try_global::<OpenMenu>()
            .is_some_and(|open| open.0 == focus)
        {
            cx.remove_global::<OpenMenu>();
        }
    })
    .detach();
    let entries: Rc<[Entry]> = entries.into();
    // Tauri focuses the item a menu opens on, which shows only when the
    // keyboard opened it (`:focus-visible`); in Kit selected is highlighted.
    if window.last_input_was_keyboard() {
        select_on_open(cx.weak_entity(), entries.clone(), window, cx);
    }
    fill_entries(menu, entries, window, cx)
}

fn fill_entries(
    menu: PopupMenu,
    entries: Rc<[Entry]>,
    window: &mut Window,
    cx: &mut Context<PopupMenu>,
) -> PopupMenu {
    if let Some(built) = cx.try_global::<Built>() {
        let this = cx.entity_id();
        let mut menus: Vec<BuiltMenu> = built
            .0
            .iter()
            .filter(|(menu, ..)| menu.entity_id() != this && menu.upgrade().is_some())
            .cloned()
            .collect();
        menus.push((cx.weak_entity(), entries.clone()));
        cx.set_global(Built(menus));
    }
    entries.iter().fold(menu, |menu, entry| match entry {
        Entry::Separator => menu.separator(),
        Entry::Item(item) => menu.item(item.popup()),
        // Kit has no disabled submenu: a disabled item says the same.
        Entry::Submenu {
            label,
            disabled: true,
            ..
        } => menu.item(PopupMenuItem::new(label.clone()).disabled(true)),
        Entry::Submenu { label, entries, .. } => {
            let entries = entries.clone();
            menu.submenu(label.clone(), window, cx, move |menu, window, cx| {
                fill_entries(menu, entries.clone(), window, cx)
            })
        }
    })
}

/// How many steps down from nothing reach the item a menu opens on: the
/// checked one, as a picker opens on what is picked, else the first. Kit
/// counts only enabled items and submenus.
fn presses_to_open_on(entries: &[Entry]) -> usize {
    let checked: Vec<bool> = entries
        .iter()
        .filter_map(|entry| match entry {
            Entry::Item(item) if !item.disabled => Some(item.checked == Some(true)),
            Entry::Submenu {
                disabled: false, ..
            } => Some(false),
            _ => None,
        })
        .collect();
    if checked.is_empty() {
        0
    } else {
        checked.iter().position(|checked| *checked).unwrap_or(0) + 1
    }
}

/// Kit opens a menu with nothing selected, and moves the selection only
/// through its actions, which reach the menu once it is drawn: one frame draws
/// it, the next selects, from nothing whatever the pointer did since. A key
/// pressed in between is left to act on its own.
fn select_on_open(
    menu: WeakEntity<PopupMenu>,
    entries: Rc<[Entry]>,
    window: &mut Window,
    cx: &App,
) {
    let presses = presses_to_open_on(&entries);
    if presses == 0 {
        return;
    }
    let keys = Keys::count(cx);
    window.on_next_frame(move |window, _| {
        window.on_next_frame(move |window, cx| {
            if let Some(menu) = menu.upgrade().filter(|_| Keys::count(cx) == keys) {
                select(&menu, entries, presses, true, window, cx);
            }
        })
    });
}

/// How many keys have been pressed, so a menu can tell whether one came
/// while it waited to select.
#[derive(Default)]
struct Keys(usize);

impl Global for Keys {}

impl Keys {
    fn count(cx: &App) -> usize {
        cx.try_global::<Self>().map_or(0, |keys| keys.0)
    }
}

/// Selects the item `steps` down (or up) from nothing. Kit steps the selection
/// only from where it is; rebuilding the menu is the only way back to nothing.
fn select(
    menu: &Entity<PopupMenu>,
    entries: Rc<[Entry]>,
    steps: usize,
    down: bool,
    window: &mut Window,
    cx: &mut App,
) {
    menu.update(cx, |menu, cx| {
        menu.rebuild(window, cx, |menu, window, cx| {
            fill_entries(menu, entries, window, cx)
        })
    });
    let focus = menu.focus_handle(cx);
    for _ in 0..steps {
        if down {
            focus.dispatch_action(&SelectDown, window, cx);
        } else {
            focus.dispatch_action(&SelectUp, window, cx);
        }
    }
}

/// Home, End and Tab in a menu [`fill`] built: down from nothing is the first
/// item, up from nothing the last. Tab steps as the arrows do, as Tauri's menu
/// kept it from walking out of the menu.
fn menu_keys(event: &KeystrokeEvent, window: &mut Window, cx: &mut App) {
    let keys = cx.default_global::<Keys>();
    keys.0 = keys.0.wrapping_add(1);
    let keystroke = &event.keystroke;
    let modifiers = keystroke.modifiers;
    let (jump, down) = match keystroke.key.as_str() {
        "home" if !modifiers.modified() => (true, true),
        "end" if !modifiers.modified() => (true, false),
        "tab" if !(modifiers.control || modifiers.alt || modifiers.platform) => {
            (false, !modifiers.shift)
        }
        _ => return,
    };
    let focused = cx.try_global::<Built>().and_then(|built| {
        built.0.iter().find_map(|(menu, entries)| {
            let menu = menu.upgrade()?;
            menu.focus_handle(cx)
                .is_focused(window)
                .then(|| (menu, entries.clone()))
        })
    });
    let Some((menu, entries)) = focused else {
        return;
    };
    if jump {
        select(&menu, entries, 1, down, window, cx);
    } else {
        let focus = menu.focus_handle(cx);
        if down {
            focus.dispatch_action(&SelectDown, window, cx);
        } else {
            focus.dispatch_action(&SelectUp, window, cx);
        }
    }
    cx.stop_propagation();
}

/// A view drawn with the keyboard's menus over it, as the shell draws them.
#[cfg(test)]
pub(crate) struct WithMenus(pub gpui_kit::AnyView);

#[cfg(test)]
impl gpui_kit::Render for WithMenus {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .child(self.0.clone())
            .children(keyboard_menu(window, cx))
    }
}

#[cfg(test)]
mod tests {
    use gpui_kit::{
        Focusable, InteractiveElement, KeyDownEvent, Render, StatefulInteractiveElement,
        TestAppContext, VisualTestContext, component::menu::ContextMenuExt, prelude::FluentBuilder,
    };

    use super::*;

    /// A surface that opens its menu from the keyboard, as each row does,
    /// recording which item ran and whether it ran with focus back on it.
    struct Surface {
        focus: FocusHandle,
        ran: Ran,
    }

    /// Each item that ran, and whether its opener held focus as it did.
    type Ran = Rc<RefCell<Vec<(&'static str, bool)>>>;

    impl Render for Surface {
        fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            let (focus, ran) = (self.focus.clone(), self.ran.clone());
            let menu: Builder = Rc::new(move |menu, window, cx| {
                let item = |label: &'static str, checked: bool| {
                    let (focus, ran) = (focus.clone(), ran.clone());
                    Entry::from(
                        Item::new(label, move |window, _| {
                            ran.borrow_mut().push((label, focus.is_focused(window)));
                        })
                        .checked(checked),
                    )
                };
                let entries = [
                    item("First", false),
                    item("Checked", true),
                    Entry::Separator,
                    item("Last", false),
                ];
                fill(menu, &entries, window, cx)
            });
            let keyed = menu.clone();
            div()
                .id("surface")
                .relative()
                .size_full()
                .track_focus(&self.focus)
                .on_key_down(move |event: &KeyDownEvent, window, cx| {
                    if !is_menu_key(&event.keystroke) {
                        return;
                    }
                    let menu = keyed.clone();
                    open(move |popup, window, cx| menu(popup, window, cx), window, cx);
                    cx.stop_propagation();
                })
                .child(anchor(&self.focus))
                .children(keyboard_menu(window, cx))
                .context_menu(move |popup, window, cx| menu(popup, window, cx))
        }
    }

    fn surface(cx: &mut TestAppContext) -> (Entity<Surface>, &mut VisualTestContext, Ran) {
        cx.update(|cx| {
            gpui_kit::init(cx);
            init(cx);
        });
        let ran = Rc::default();
        let (view, visual) = cx.add_window_view({
            let ran = Rc::clone(&ran);
            move |_, cx| Surface {
                focus: cx.focus_handle(),
                ran,
            }
        });
        visual.update(|window, cx| {
            let focus = view.read(cx).focus.clone();
            window.focus(&focus, cx);
        });
        (view, visual, ran)
    }

    /// Opens the menu by `key` and lets the two frames that place the
    /// selection pass.
    fn open_by(key: &str, visual: &mut VisualTestContext) {
        visual.simulate_keystrokes(key);
        settle(visual);
    }

    /// Lets the two frames that place the selection pass.
    fn settle(visual: &mut VisualTestContext) {
        for _ in 0..2 {
            visual.update(|window, cx| {
                window.refresh();
                window.simulate_next_frame(cx);
            });
            visual.run_until_parked();
        }
    }

    #[gpui_kit::test]
    fn opens_from_the_keyboard_on_its_checked_item_and_hands_focus_back(cx: &mut TestAppContext) {
        let (view, visual, ran) = surface(cx);
        for key in ["shift-f10", "menu"] {
            open_by(key, visual);
            visual.update(|window, cx| {
                assert!(is_open(window, cx), "{key} opens the menu");
                assert!(!view.read(cx).focus.is_focused(window));
            });
            visual.simulate_keystrokes("enter");
            visual.update(|window, cx| {
                assert!(!is_open(window, cx));
                assert!(view.read(cx).focus.is_focused(window));
            });
        }
        assert_eq!(
            *ran.borrow(),
            [("Checked", true), ("Checked", true)],
            "each opening lands on the checked item, and runs it from the opener"
        );
    }

    #[gpui_kit::test]
    fn a_menu_the_pointer_opens_has_nothing_selected(cx: &mut TestAppContext) {
        let (_, visual, ran) = surface(cx);
        let at = gpui_kit::point(px(10.), px(10.));
        visual.simulate_event(gpui_kit::MouseDownEvent {
            button: gpui_kit::MouseButton::Right,
            position: at,
            modifiers: Default::default(),
            click_count: 1,
            first_mouse: false,
        });
        visual.simulate_event(gpui_kit::MouseUpEvent {
            button: gpui_kit::MouseButton::Right,
            position: at,
            modifiers: Default::default(),
            click_count: 1,
        });
        settle(visual);
        visual.update(|window, cx| assert!(is_open(window, cx)));
        // As Tauri's, which shows where it opened only to the keyboard.
        visual.simulate_keystrokes("enter");
        visual.update(|window, cx| assert!(is_open(window, cx)));
        assert!(ran.borrow().is_empty(), "Enter has nothing to run");
        visual.simulate_keystrokes("shift-f10");
        visual.update(|_, cx| assert!(!cx.has_global::<KeyboardMenu>(), "one menu at a time"));
    }

    /// A dropdown, which Kit builds in a popover rather than through [`open`].
    struct Dropdown(Ran);

    impl Render for Dropdown {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            use gpui_kit::component::{button::Button, menu::DropdownMenu};
            let ran = self.0.clone();
            Button::new("picker")
                .label("Pick")
                .dropdown_menu(move |menu, window, cx| {
                    let item = |label: &'static str, checked: bool| {
                        let ran = ran.clone();
                        Entry::from(
                            Item::new(label, move |_, _| ran.borrow_mut().push((label, true)))
                                .checked(checked),
                        )
                    };
                    let entries = [
                        item("First", false),
                        item("Checked", true),
                        item("Last", false),
                    ];
                    fill(menu, &entries, window, cx)
                })
        }
    }

    #[gpui_kit::test]
    fn a_dropdown_the_keyboard_opens_lands_on_its_checked_item(cx: &mut TestAppContext) {
        use gpui_kit::test::TestWindowExt as _;
        cx.update(|cx| {
            gpui_kit::init(cx);
            init(cx);
        });
        let ran = Ran::default();
        let (_, visual) = cx.add_window_view({
            let ran = ran.clone();
            move |_, _| Dropdown(ran)
        });
        visual.update(|window, cx| window.focus_next(cx));
        visual.update(|window, cx| window.press("enter", cx));
        settle(visual);
        settle(visual);
        visual.simulate_keystrokes("enter");
        assert_eq!(*ran.borrow(), [("Checked", true)]);
    }

    #[gpui_kit::test]
    fn home_and_end_select_the_first_and_last_items(cx: &mut TestAppContext) {
        let (_, visual, ran) = surface(cx);
        open_by("shift-f10", visual);
        visual.simulate_keystrokes("end enter");
        open_by("shift-f10", visual);
        visual.simulate_keystrokes("end home enter");
        assert_eq!(*ran.borrow(), [("Last", true), ("First", true)]);
    }

    #[gpui_kit::test]
    fn a_key_pressed_as_the_menu_opens_is_not_undone(cx: &mut TestAppContext) {
        let (_, visual, ran) = surface(cx);
        open_by("shift-f10 end", visual);
        visual.simulate_keystrokes("enter");
        assert_eq!(*ran.borrow(), [("Last", true)]);
    }

    #[gpui_kit::test]
    fn tab_steps_through_the_items_and_stays_in_the_menu(cx: &mut TestAppContext) {
        let (_, visual, ran) = surface(cx);
        // From the checked item on to the last and round to the first, then
        // back round through the last and the checked item to the first.
        open_by("shift-f10", visual);
        visual.simulate_keystrokes("tab tab shift-tab shift-tab shift-tab enter");
        visual.update(|window, cx| assert!(!is_open(window, cx)));
        assert_eq!(*ran.borrow(), [("First", true)]);
    }

    /// Ten rows in a list that shows two.
    struct Rows {
        rows: Vec<FocusHandle>,
        scroll: ScrollHandle,
    }

    impl Render for Rows {
        fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            let rows: Vec<_> = self
                .rows
                .iter()
                .enumerate()
                .map(|(ix, focus)| {
                    reveal_when_focused(format!("row-{ix}"), focus, &self.scroll, window, cx);
                    div()
                        .id(ix)
                        .relative()
                        .h(px(50.))
                        .track_focus(focus)
                        .when(focus.is_focused(window), |row| row.child(anchor(focus)))
                })
                .collect();
            div()
                .id("rows")
                .h(px(100.))
                .overflow_y_scroll()
                .track_scroll(&self.scroll)
                .children(rows)
        }
    }

    #[gpui_kit::test]
    fn a_row_that_takes_the_keyboard_scrolls_into_view(cx: &mut TestAppContext) {
        cx.update(init);
        let (view, visual) = cx.add_window_view(|_, cx| Rows {
            rows: (0..10).map(|_| cx.focus_handle()).collect(),
            scroll: ScrollHandle::new(),
        });
        visual.update(|window, _| window.activate_window());
        settle(visual);
        let offset =
            |visual: &mut VisualTestContext| visual.update(|_, cx| view.read(cx).scroll.offset().y);
        assert_eq!(offset(visual), px(0.));
        // As a key moves it, which a press does not.
        visual.simulate_keystrokes("x");
        visual.update(|window, cx| view.read(cx).rows[8].clone().focus(window, cx));
        settle(visual);
        // Row 8 spans 400 to 450: its bottom meets the list's.
        assert_eq!(offset(visual), px(-350.));
        visual.update(|window, cx| view.read(cx).rows[1].clone().focus(window, cx));
        settle(visual);
        assert_eq!(offset(visual), px(-50.));
    }

    #[gpui_kit::test]
    fn a_menu_counts_as_open_while_it_holds_focus(cx: &mut TestAppContext) {
        let visual = cx.add_empty_window();
        let menu = visual.update(|window, cx| {
            gpui_kit::init(cx);
            assert!(!is_open(window, cx));
            let menu = PopupMenu::build(window, cx, |menu, window, cx| {
                fill(menu, &[Entry::Separator], window, cx)
            });
            menu.focus_handle(cx).focus(window, cx);
            assert!(is_open(window, cx));
            window.blur(cx);
            assert!(!is_open(window, cx));

            // Closed with nothing focused before it: focus stays on it.
            menu.focus_handle(cx).focus(window, cx);
            menu.update(cx, |_, cx| cx.emit(DismissEvent));
            menu
        });
        visual.update(|window, cx| {
            assert!(menu.focus_handle(cx).is_focused(window));
            assert!(!is_open(window, cx));
        });
    }

    #[test]
    fn a_disabled_destructive_item_is_drawn_as_disabled() {
        let item = |disabled| {
            Item::new("Delete…", |_, _| {})
                .destructive()
                .disabled(disabled)
        };
        assert!(matches!(
            item(false).popup(),
            PopupMenuItem::ElementItem {
                disabled: false,
                ..
            }
        ));
        assert!(matches!(
            item(true).popup(),
            PopupMenuItem::Item { disabled: true, .. }
        ));
    }

    #[test]
    fn the_menu_key_is_shift_f10_or_the_menu_key() {
        let key = |text: &'static str| is_menu_key(&Keystroke::parse(text).unwrap());
        assert!(key("shift-f10"));
        assert!(key("menu"));
        assert!(!key("f10"), "F10 alone is the terminal's");
        assert!(!key("shift-f9"));
    }

    #[test]
    fn a_menu_opens_on_its_checked_item_or_its_first() {
        let item = |checked: Option<bool>, disabled| {
            let item = Item::new("x", |_, _| {}).disabled(disabled);
            Entry::Item(match checked {
                Some(checked) => item.checked(checked),
                None => item,
            })
        };
        assert_eq!(presses_to_open_on(&[]), 0);
        assert_eq!(
            presses_to_open_on(&[item(None, true)]),
            0,
            "nothing to land on"
        );
        assert_eq!(
            presses_to_open_on(&[item(None, true), Entry::Separator, item(None, false)]),
            1,
            "the first enabled item, whatever stands before it"
        );
        assert_eq!(
            presses_to_open_on(&[
                item(Some(false), false),
                item(Some(true), true),
                Entry::Separator,
                item(Some(false), false),
                item(Some(true), false),
            ]),
            3,
            "the checked one, counted among the items Kit can select"
        );
    }
}
