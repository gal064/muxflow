//! The macOS application menu.
//!
//! Tauri installs [`Menu::default`] on macOS when a builder sets no menu of its
//! own, and that menu carries a close-window item in both its File and Window
//! submenus, each bound to ⌘W. Muxflow binds ⌘W itself, to "Close current tab".
//! Whenever the frontend left the keystroke unclaimed — no tab to close, a host
//! link not yet connected, an overlay holding focus — macOS answered the
//! unclaimed ⌘W with its own binding and closed the window. There is only one
//! window, so the app exited in the middle of a session, cleanly enough that no
//! crash report was ever written.
//!
//! So macOS gets a menu built here instead: Tauri's default, item for item,
//! minus every close-window entry. Quitting stays where macOS users expect it —
//! ⌘Q, and the red button.
//!
//! This menu is also the whole of the defence. The frontend swallows a claimed
//! chord it cannot act on, but it still yields ⌘W to the platform whenever an
//! overlay holds focus, a text field has focus, or a user rebinding leaves the
//! chord ambiguous — every one of which was a way to lose the window before.
//! Nothing is left for those to reach.
//!
//! This module is macOS-only, and so is the builder call that installs it. No
//! other platform has the collision — Linux binds that command to Ctrl+W, but
//! there is no menu there to claim it too — and none has a menu to correct:
//! Tauri's default is itself macOS-gated, so a menu here would put a bar on
//! Linux windows that never had one.

use tauri::menu::{
    AboutMetadata, HELP_SUBMENU_ID, IsMenuItem, Menu, PredefinedMenuItem, Submenu,
    WINDOW_SUBMENU_ID,
};
use tauri::{AppHandle, Wry};

/// A menu entry Muxflow is willing to show.
///
/// Deliberately without a `CloseWindow`: see the module comment. The omission
/// is a property of the type rather than a rule to remember.
#[derive(Clone, Copy)]
enum Item {
    About,
    Separator,
    Services,
    Hide,
    HideOthers,
    Quit,
    Undo,
    Redo,
    Cut,
    Copy,
    Paste,
    SelectAll,
    Fullscreen,
    Minimize,
    Maximize,
}

fn item(
    app: &AppHandle,
    entry: Item,
    about: &AboutMetadata,
) -> tauri::Result<PredefinedMenuItem<Wry>> {
    match entry {
        Item::About => PredefinedMenuItem::about(app, None, Some(about.clone())),
        Item::Separator => PredefinedMenuItem::separator(app),
        Item::Services => PredefinedMenuItem::services(app, None),
        Item::Hide => PredefinedMenuItem::hide(app, None),
        Item::HideOthers => PredefinedMenuItem::hide_others(app, None),
        Item::Quit => PredefinedMenuItem::quit(app, None),
        Item::Undo => PredefinedMenuItem::undo(app, None),
        Item::Redo => PredefinedMenuItem::redo(app, None),
        Item::Cut => PredefinedMenuItem::cut(app, None),
        Item::Copy => PredefinedMenuItem::copy(app, None),
        Item::Paste => PredefinedMenuItem::paste(app, None),
        Item::SelectAll => PredefinedMenuItem::select_all(app, None),
        Item::Fullscreen => PredefinedMenuItem::fullscreen(app, None),
        Item::Minimize => PredefinedMenuItem::minimize(app, None),
        Item::Maximize => PredefinedMenuItem::maximize(app, None),
    }
}

fn submenu(
    app: &AppHandle,
    id: Option<&str>,
    title: &str,
    items: &[Item],
    about: &AboutMetadata,
) -> tauri::Result<Submenu<Wry>> {
    let built = items
        .iter()
        .map(|entry| item(app, *entry, about))
        .collect::<tauri::Result<Vec<_>>>()?;
    let refs = built
        .iter()
        .map(|entry| entry as &dyn IsMenuItem<Wry>)
        .collect::<Vec<_>>();
    match id {
        Some(id) => Submenu::with_id_and_items(app, id, title, true, &refs),
        None => Submenu::with_items(app, title, true, &refs),
    }
}

/// Build the menu Tauri installs at startup.
pub fn build(app: &AppHandle) -> tauri::Result<Menu<Wry>> {
    let package = app.package_info();
    let config = app.config();
    let about = AboutMetadata {
        name: Some(package.name.clone()),
        version: Some(package.version.to_string()),
        copyright: config.bundle.copyright.clone(),
        authors: config
            .bundle
            .publisher
            .clone()
            .map(|publisher| vec![publisher]),
        ..Default::default()
    };

    let application = submenu(
        app,
        None,
        &package.name,
        &[
            Item::About,
            Item::Separator,
            Item::Services,
            Item::Separator,
            Item::Hide,
            Item::HideOthers,
            Item::Separator,
            Item::Quit,
        ],
        &about,
    )?;
    // The Edit submenu is not decoration. Muxflow leaves ⌘C/⌘V alone whenever
    // an ordinary text field or a selectable document surface has focus — the
    // terminal's own copy and paste are bound to those chords and must not
    // steal them — so native editing depends on these items to answer.
    let edit = submenu(
        app,
        None,
        "Edit",
        &[
            Item::Undo,
            Item::Redo,
            Item::Separator,
            Item::Cut,
            Item::Copy,
            Item::Paste,
            Item::SelectAll,
        ],
        &about,
    )?;
    // Full screen pairs with `macos_window::enable_native_full_screen`, which
    // opts the window into the real thing rather than a zoom.
    let view = submenu(app, None, "View", &[Item::Fullscreen], &about)?;
    // Keeping Tauri's id is what lets it hand this submenu to
    // `setWindowsMenu:`, so AppKit keeps managing the window list inside it.
    let window = submenu(
        app,
        Some(WINDOW_SUBMENU_ID),
        "Window",
        &[Item::Minimize, Item::Maximize],
        &about,
    )?;
    // Empty on purpose, exactly as Tauri's default leaves it: the id is what
    // makes AppKit adopt it with `setHelpMenu:` and fill it with the standard
    // searchable Help, which is lost if the submenu is simply absent.
    let help = submenu(app, Some(HELP_SUBMENU_ID), "Help", &[], &about)?;

    Menu::with_items(app, &[&application, &edit, &view, &window, &help])
}
