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
    App, IntoElement, ParentElement, SharedString, Styled, Window,
    component::menu::{PopupMenu, PopupMenuItem},
    div,
};

use crate::{commands::registry::CommandId, menus::RunCommand, theme};

type Run = Rc<dyn Fn(&mut Window, &mut App)>;

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

pub enum Entry {
    Item(Item),
    Separator,
}

impl From<Item> for Entry {
    fn from(item: Item) -> Self {
        Self::Item(item)
    }
}

/// Fills `menu` with `entries`.
pub fn fill(menu: PopupMenu, entries: &[Entry]) -> PopupMenu {
    entries.iter().fold(menu, |menu, entry| match entry {
        Entry::Separator => menu.separator(),
        Entry::Item(item) => menu.item(item.popup()),
    })
}
