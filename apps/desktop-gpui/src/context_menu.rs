//! Context menus, on Kit's `PopupMenu`. Port of `apps/desktop/src/ui/ContextMenu.tsx`.
//!
//! Attach a menu to an element with Kit's `.context_menu(...)`, and fill it with
//! [`fill`]. Kit supplies what the TypeScript menu wrote by hand: arrow keys,
//! Home and End, Escape, dismissal on an outside press, and keeping the menu
//! inside the window. A shortcut shows when an item names its command, and
//! follows the overrides ([`crate::menus`] keeps the bindings it reads).
//!
//! Not carried over: opening from the keyboard (Shift+F10 or the menu key). Kit
//! has no way to open a context menu except a right press or a long press.

use std::rc::Rc;

use gpui_kit::{
    App, Context, DismissEvent, FocusHandle, Focusable, Global, IntoElement, ParentElement,
    SharedString, Styled, Window,
    component::menu::{PopupMenu, PopupMenuItem},
    div,
};

use crate::{commands::registry::CommandId, menus::RunCommand, theme};

type Run = Rc<dyn Fn(&mut Window, &mut App)>;

#[derive(Clone)]
pub struct Item {
    label: SharedString,
    disabled: bool,
    destructive: bool,
    checked: Option<bool>,
    command: Option<CommandId>,
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
            command: None,
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

    /// Shows this command's shortcut beside the label.
    pub fn command(mut self, command: CommandId) -> Self {
        self.command = Some(command);
        self
    }

    fn popup(&self) -> PopupMenuItem {
        let label = self.label.clone();
        let item = if self.destructive {
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
            .on_click(move |_, window, cx| run(window, cx))
            .checked(self.checked.unwrap_or(false));
        match self.command {
            // Only the hint: a handler runs the item.
            Some(id) => item.action(Box::new(RunCommand { id })),
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

/// Fills `menu` with `entries`.
pub fn fill(
    menu: PopupMenu,
    entries: &[Entry],
    window: &mut Window,
    cx: &mut Context<PopupMenu>,
) -> PopupMenu {
    let focus = menu.focus_handle(cx);
    cx.set_global(OpenMenu(focus));
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
    fill_entries(menu, entries, window, cx)
}

fn fill_entries(
    menu: PopupMenu,
    entries: &[Entry],
    window: &mut Window,
    cx: &mut Context<PopupMenu>,
) -> PopupMenu {
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
                fill_entries(menu, &entries, window, cx)
            })
        }
    })
}

#[cfg(test)]
mod tests {
    use gpui_kit::{Focusable, TestAppContext};

    use super::*;

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
}
