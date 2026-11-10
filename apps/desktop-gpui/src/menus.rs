//! The application menu: native on macOS (`set_menus`), in the title bar on
//! Linux. Port of `menu.rs`, whose macOS menu was Tauri's default minus every
//! close-window item. There is none here either: only one window exists, and
//! ⌘W belongs to "Close current tab".
//!
//! Commands run through `intercept_keystrokes`, not GPUI's keymap, so a menu
//! item is a [`RunCommand`] action and its accelerator is a binding made only to
//! be shown. Each binding sits under `!Root`, which no real key press matches
//! (every dispatch path passes through Kit's `Root`), so it never fires on its
//! own. macOS reads it for the key equivalent, Kit's popup for the hint. The
//! bindings are rebuilt when the overrides change.
//!
//! A key equivalent the app did not claim still reaches the menu on macOS, which
//! is how Cut, Copy and Paste reach a focused field.
//!
//! Not ported: the About item and Help's searchable menu, which need the
//! product version and AppKit's help book.

use std::rc::Rc;

use gpui_kit::{
    Action, App, Global, KeyBinding, KeyBindingContextPredicate, Menu, MenuItem, OsAction,
    OwnedMenuItem, SystemMenuType, Window, actions,
    component::{
        IconName, WindowExt,
        input::{Copy, Cut, Paste, Redo, SelectAll, Undo},
        menu::{DropdownMenu, PopupMenu, PopupMenuItem},
    },
};

use crate::{
    commands::{
        self, Commands,
        keymap::{normalize_shortcut, shortcut_for},
        registry::{COMMANDS, CommandId, Platform, command_by_id},
    },
    ui::title_bar::bar_button,
};

actions!(
    muxflow,
    [Hide, HideOthers, Quit, Minimize, Zoom, ToggleFullScreen]
);

/// Runs one registry command. Only a menu dispatches it.
#[derive(Clone, PartialEq, Debug, Action)]
#[action(namespace = muxflow, no_json)]
pub struct RunCommand {
    pub id: CommandId,
}

/// The commands the menus offer, by menu. Others stay in the palette.
const SETTINGS: &[CommandId] = &["settings.show"];
const VIEW: &[CommandId] = &[
    "commands.show",
    "workspaces.switch",
    "view.toggleSidebar",
    "view.togglePanel",
];

fn command(commands: &Commands, id: CommandId, name: &str) -> MenuItem {
    let definition = command_by_id(id).expect("a menu names a known command");
    MenuItem::action(name, RunCommand { id }).disabled(!commands.is_available(definition))
}

/// The menus for `platform`.
pub fn app_menus(commands: &Commands, platform: Platform) -> Vec<Menu> {
    let item = |id: CommandId| {
        let title = command_by_id(id).map_or(id, |definition| definition.title);
        command(commands, id, title)
    };
    let mut menus = Vec::new();
    if platform == Platform::Mac {
        menus.push(Menu::new("Muxflow").items([
            command(commands, SETTINGS[0], "Settings…"),
            MenuItem::separator(),
            MenuItem::os_submenu("Services", SystemMenuType::Services),
            MenuItem::separator(),
            MenuItem::action("Hide Muxflow", Hide),
            MenuItem::action("Hide Others", HideOthers),
            MenuItem::separator(),
            MenuItem::action("Quit Muxflow", Quit),
        ]));
    }
    // The Edit items answer for fields: Muxflow leaves Copy and Paste to them
    // whenever a text field has focus, so native editing depends on these.
    menus.push(Menu::new("Edit").items([
        MenuItem::os_action("Undo", Undo, OsAction::Undo),
        MenuItem::os_action("Redo", Redo, OsAction::Redo),
        MenuItem::separator(),
        MenuItem::os_action("Cut", Cut, OsAction::Cut),
        MenuItem::os_action("Copy", Copy, OsAction::Copy),
        MenuItem::os_action("Paste", Paste, OsAction::Paste),
        MenuItem::os_action("Select All", SelectAll, OsAction::SelectAll),
    ]));
    let mut view: Vec<MenuItem> = VIEW.iter().map(|id| item(id)).collect();
    view.push(MenuItem::separator());
    view.push(MenuItem::action("Toggle Full Screen", ToggleFullScreen));
    menus.push(Menu::new("View").items(view));
    menus.push(Menu::new("Window").items([
        MenuItem::action("Minimize", Minimize),
        MenuItem::action("Zoom", Zoom),
    ]));
    menus
}

/// `Meta+Shift+E` as GPUI writes it, or `None` for a key it has no name for.
fn gpui_keystroke(shortcut: &str) -> Option<String> {
    let normalized = normalize_shortcut(shortcut);
    let (key, modifiers) = normalized.rsplit_once('+').map_or_else(
        || (normalized.as_str(), Vec::new()),
        |(modifiers, key)| (key, modifiers.split('+').collect()),
    );
    let key = match key {
        "" => return None,
        "ArrowLeft" => "left".to_owned(),
        "ArrowRight" => "right".to_owned(),
        "ArrowUp" => "up".to_owned(),
        "ArrowDown" => "down".to_owned(),
        "PageUp" => "pageup".to_owned(),
        "PageDown" => "pagedown".to_owned(),
        key if key.chars().count() == 1
            || matches!(
                key,
                "Enter" | "Escape" | "Tab" | "Space" | "Backspace" | "Delete" | "Home" | "End"
            )
            || key
                .strip_prefix('F')
                .is_some_and(|n| n.parse::<u8>().is_ok()) =>
        {
            key.to_lowercase()
        }
        _ => return None,
    };
    let mut parts: Vec<&str> = modifiers
        .iter()
        .map(|modifier| match *modifier {
            "Ctrl" => "ctrl",
            "Alt" => "alt",
            "Shift" => "shift",
            _ => "cmd",
        })
        .collect();
    parts.push(&key);
    Some(parts.join("-"))
}

/// What the menus last showed, so a context change that touches none of it does
/// not rebuild them. Every command's shortcut is kept as a binding, so any
/// menu can show it; only the menu commands' availability is.
#[derive(Default, PartialEq)]
struct Shown {
    shortcuts: Vec<(CommandId, String)>,
    available: Vec<bool>,
}

impl Global for Shown {}

fn shown(commands: &Commands) -> Shown {
    let (platform, overrides) = (commands.platform(), commands.overrides());
    Shown {
        shortcuts: COMMANDS
            .iter()
            .filter_map(|command| {
                let shortcut = shortcut_for(command, platform, overrides)?;
                Some((command.id, gpui_keystroke(shortcut)?))
            })
            .collect(),
        available: SETTINGS
            .iter()
            .chain(VIEW)
            .map(|id| {
                commands.is_available(command_by_id(id).expect("a menu names a known command"))
            })
            .collect(),
    }
}

/// Replaces the accelerator bindings in the keymap with ones for the current
/// shortcuts, leaving every other binding as it was.
fn sync_accelerators(shortcuts: &[(CommandId, String)], cx: &mut App) {
    let inert = Rc::new(KeyBindingContextPredicate::parse("!Root").expect("a valid context"));
    let accelerators: Vec<KeyBinding> = shortcuts
        .iter()
        .filter_map(|(id, keystroke)| {
            KeyBinding::load(
                keystroke,
                Box::new(RunCommand { id }),
                Some(inert.clone()),
                false,
                None,
                cx,
            )
            .ok()
        })
        .collect();
    let keymap = cx.key_bindings();
    let mut keymap = keymap.borrow_mut();
    let others: Vec<KeyBinding> = keymap
        .bindings()
        .filter(|binding| {
            binding
                .action()
                .as_any()
                .downcast_ref::<RunCommand>()
                .is_none()
        })
        .cloned()
        .collect();
    keymap.clear();
    keymap.add_bindings(others);
    keymap.add_bindings(accelerators);
}

fn refresh(cx: &mut App) {
    let commands = cx.global::<Commands>();
    let now = shown(commands);
    if cx.try_global::<Shown>() == Some(&now) {
        return;
    }
    let menus = app_menus(commands, commands.platform());
    if cx
        .try_global::<Shown>()
        .is_none_or(|last| last.shortcuts != now.shortcuts)
    {
        sync_accelerators(&now.shortcuts, cx);
    }
    cx.set_menus(menus);
    cx.set_global(now);
    cx.refresh_windows();
}

/// Runs `act` in the active window once the action that asked has finished. A
/// menu item reaches here from inside that window's own dispatch, while the
/// window is borrowed and cannot be updated.
fn in_active_window(cx: &mut App, act: impl FnOnce(&mut Window, &mut App) + 'static) {
    cx.defer(move |cx| {
        if let Some(handle) = cx.active_window() {
            handle.update(cx, |_, window, cx| act(window, cx)).ok();
        }
    });
}

/// An application menu's actions, which have no owner in the element tree.
fn on_actions(cx: &mut App) {
    cx.on_action(|_: &Hide, cx| cx.hide());
    cx.on_action(|_: &HideOthers, cx| cx.hide_other_apps());
    cx.on_action(|_: &Quit, cx| cx.quit());
    cx.on_action(|_: &Minimize, cx| in_active_window(cx, |window, _| window.minimize_window()));
    cx.on_action(|_: &Zoom, cx| in_active_window(cx, |window, _| window.zoom_window()));
    cx.on_action(|_: &ToggleFullScreen, cx| {
        in_active_window(cx, |window, _| window.toggle_fullscreen());
    });
    cx.on_action(|run: &RunCommand, cx| {
        let id = run.id;
        in_active_window(cx, move |window, cx| {
            let definition = command_by_id(id).expect("a menu names a known command");
            // A key equivalent macOS passes on while a dialog is open belongs to
            // the dialog, as every other shortcut does.
            if !window.has_active_dialog(cx) && cx.global::<Commands>().is_available(definition) {
                commands::run(id, window, cx);
            }
        });
    });
}

/// Called once from [`crate::features::init`].
pub fn init(cx: &mut App) {
    on_actions(cx);
    if cfg!(target_os = "macos") {
        // The application menu's own items take their key equivalents from the
        // keymap, as the commands' do (see the module comment on `!Root`).
        cx.bind_keys([
            KeyBinding::new("cmd-q", Quit, Some("!Root")),
            KeyBinding::new("cmd-h", Hide, Some("!Root")),
            KeyBinding::new("alt-cmd-h", HideOthers, Some("!Root")),
            KeyBinding::new("cmd-m", Minimize, Some("!Root")),
            KeyBinding::new("ctrl-cmd-f", ToggleFullScreen, Some("!Root")),
        ]);
    }
    refresh(cx);
    cx.observe_global::<Commands>(refresh).detach();
}

/// The menus as a popup, for Linux: each top-level menu is a submenu.
fn popup(menu: PopupMenu, window: &mut Window, cx: &mut gpui_kit::Context<PopupMenu>) -> PopupMenu {
    let menus = cx.get_menus().unwrap_or_default();
    menus.into_iter().fold(menu, |menu, owned| {
        let items = owned.items;
        menu.submenu(owned.name, window, cx, move |submenu, _, _| {
            items.iter().fold(submenu, |submenu, item| match item {
                OwnedMenuItem::Separator => submenu.separator(),
                OwnedMenuItem::Action {
                    name,
                    action,
                    disabled,
                    ..
                } => submenu.item(
                    PopupMenuItem::new(name.clone())
                        .action(action.boxed_clone())
                        .disabled(*disabled),
                ),
                OwnedMenuItem::Submenu(_) | OwnedMenuItem::SystemMenu(_) => submenu,
            })
        })
    })
}

/// The title bar's button for the menus, on Linux, where no native menu bar
/// exists.
pub fn menu_button(cx: &App) -> impl gpui_kit::IntoElement + use<> {
    let ink = crate::theme::palette(cx).chrome_dim;
    bar_button("app-menu", IconName::Menu, "Main menu", ink).dropdown_menu(popup)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shortcuts_are_written_the_way_gpui_reads_them() {
        for (shortcut, expected) in [
            ("Ctrl+K", Some("ctrl-k")),
            ("Meta+Shift+E", Some("shift-cmd-e")),
            ("Ctrl+Alt+ArrowLeft", Some("ctrl-alt-left")),
            ("Meta+PageDown", Some("cmd-pagedown")),
            ("Ctrl+Shift+Enter", Some("ctrl-shift-enter")),
            ("Ctrl+F5", Some("ctrl-f5")),
            ("Ctrl+,", Some("ctrl-,")),
            ("Ctrl+Whatever", None),
            ("", None),
        ] {
            assert_eq!(gpui_keystroke(shortcut).as_deref(), expected, "{shortcut}");
        }
    }
}
