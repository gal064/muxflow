//! The native surfaces through GPUI's test platform: menus and their
//! accelerators, notices, tooltips, context menus and window geometry.

use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    path::{Path, PathBuf},
    rc::Rc,
    time::Duration,
};

use desktop_core::app_state::{AppStateStore, WindowGeometry, load_app_state};
use gpui_kit::{
    AnyWindowHandle, App, AppContext, Bounds, Context, InteractiveElement, IntoElement,
    OperatingSystem, OwnedMenuItem, ParentElement, Render, Role, Styled, TestAppContext,
    TestSupportExt, Window, WindowBounds, WindowOptions,
    component::{WindowExt, menu::ContextMenuExt},
    div, point, px, size,
    test::{TestQueryExt, TestWindowExt},
};
use muxflow_gpui::{
    app::{self, Core},
    commands::{self, Commands, keymap::ShortcutOverrides, registry::Platform},
    context_menu::{self, Entry, Item},
    menus::{self, RunCommand},
    notices,
    ui::shell::Shell,
};

const OS: OperatingSystem = if cfg!(target_os = "macos") {
    OperatingSystem::Mac
} else {
    OperatingSystem::Linux
};

fn write_state(dir: &Path, geometry: Option<serde_json::Value>) -> PathBuf {
    let path = dir.join("app-state.json");
    let mut shell = serde_json::json!({});
    if let Some(geometry) = geometry {
        shell["windowGeometry"] = geometry;
    }
    let state = serde_json::json!({
        "schemaVersion": 1,
        "appTabs": [],
        "workspaceUi": [],
        "shell": shell,
    });
    std::fs::write(&path, serde_json::to_vec(&state).unwrap()).unwrap();
    path
}

fn boot(state: &Path, cx: &mut TestAppContext) {
    cx.update(|cx| {
        gpui_kit::init(cx);
        app::init(Core::at(state), cx);
        cx.set_reduce_motion(true);
    });
}

fn open_shell(state: &Path, width: f32, cx: &mut TestAppContext) -> AnyWindowHandle {
    boot(state, cx);
    let handle = cx.update(|cx| {
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds {
                origin: point(px(0.), px(0.)),
                size: size(px(width), px(800.)),
            })),
            ..Default::default()
        };
        gpui_kit::open_window(options, cx, Shell::open)
            .expect("open a test window")
            .0
    });
    // Menu actions go to the active window.
    with_window(handle, cx, |window, _| window.activate_window());
    handle
}

fn app_window(cx: &mut TestAppContext) -> (tempfile::TempDir, AnyWindowHandle) {
    let dir = tempfile::tempdir().unwrap();
    let path = write_state(dir.path(), None);
    let handle = open_shell(&path, 1000., cx);
    (dir, handle)
}

fn with_window<R>(
    handle: AnyWindowHandle,
    cx: &mut TestAppContext,
    action: impl FnOnce(&mut Window, &mut App) -> R,
) -> R {
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        action(window, cx)
    })
    .unwrap()
}

fn menu_names(platform: Platform, cx: &mut TestAppContext) -> Vec<String> {
    cx.update(|cx| {
        menus::app_menus(cx.global::<Commands>(), platform)
            .into_iter()
            .map(|menu| menu.name.to_string())
            .collect()
    })
}

/// Every named action in the installed menus, with whether it is disabled.
fn installed_items(cx: &mut TestAppContext) -> Vec<(String, bool)> {
    cx.update(|cx| {
        let mut items = Vec::new();
        for menu in cx.get_menus().expect("menus are installed") {
            for item in menu.items {
                if let OwnedMenuItem::Action { name, disabled, .. } = item {
                    items.push((name.to_string(), disabled));
                }
            }
        }
        items
    })
}

fn accelerators(id: &'static str, cx: &mut TestAppContext) -> Vec<String> {
    cx.update(|cx| {
        cx.key_bindings()
            .borrow()
            .bindings_for_action(&RunCommand { id })
            .map(|binding| binding.keystrokes()[0].inner().unparse(OS))
            .collect()
    })
}

fn other_bindings(cx: &mut TestAppContext) -> usize {
    cx.update(|cx| {
        cx.key_bindings()
            .borrow()
            .bindings()
            .filter(|binding| {
                binding
                    .action()
                    .as_any()
                    .downcast_ref::<RunCommand>()
                    .is_none()
            })
            .count()
    })
}

/// Clicks the one element with this accessible name, whatever its id is.
fn click_label(window: &mut Window, label: &str, cx: &mut App) {
    let id = window
        .find_by_label(label)
        .path()
        .last()
        .cloned()
        .expect("an element with an id");
    window.click(id, cx);
}

#[gpui_kit::test]
fn the_menus_follow_the_platform_and_never_close_the_window(cx: &mut TestAppContext) {
    let (_dir, _handle) = app_window(cx);
    assert_eq!(
        menu_names(Platform::Mac, cx),
        ["Muxflow", "Edit", "View", "Window"]
    );
    assert_eq!(menu_names(Platform::Linux, cx), ["Edit", "View", "Window"]);
    for platform in [Platform::Mac, Platform::Linux] {
        let closes = cx.update(|cx| {
            menus::app_menus(cx.global::<Commands>(), platform)
                .into_iter()
                .flat_map(|menu| menu.items)
                .filter(|item| {
                    matches!(item, gpui_kit::MenuItem::Action { name, .. }
                        if name.to_lowercase().contains("close"))
                })
                .count()
        });
        assert_eq!(closes, 0, "{platform:?}");
    }
    // The installed menus are the current platform's.
    let installed: Vec<String> = cx.update(|cx| {
        cx.get_menus()
            .unwrap()
            .into_iter()
            .map(|menu| menu.name.to_string())
            .collect()
    });
    assert_eq!(installed, menu_names(Platform::current(), cx));
}

#[gpui_kit::test]
fn a_menu_item_is_enabled_once_its_command_has_a_handler(cx: &mut TestAppContext) {
    let (_dir, _handle) = app_window(cx);
    let item = |cx: &mut TestAppContext, name: &str| {
        installed_items(cx)
            .into_iter()
            .find(|(item, _)| item == name)
            .unwrap_or_else(|| panic!("no menu item {name}"))
            .1
    };
    // The chrome answers the view commands, the palette answers its own.
    assert!(!item(cx, "Toggle sidebar"));
    assert!(!item(cx, "Show command palette"));
    // Nothing handles Settings yet. It sits in the macOS application menu, which
    // the installed Linux menus do not have, so read the model directly.
    let settings_disabled = |cx: &mut TestAppContext| {
        cx.update(|cx| {
            menus::app_menus(cx.global::<Commands>(), Platform::Mac)
                .into_iter()
                .flat_map(|menu| menu.items)
                .find_map(|item| match item {
                    gpui_kit::MenuItem::Action { name, disabled, .. } if name == "Settings…" => {
                        Some(disabled)
                    }
                    _ => None,
                })
                .expect("Settings is in the application menu")
        })
    };
    assert!(settings_disabled(cx));
    cx.update(|cx| commands::on_command("settings.show", cx, |_, _| {}));
    assert!(!settings_disabled(cx));
}

#[gpui_kit::test]
fn accelerators_follow_the_overrides_and_keep_the_other_bindings(cx: &mut TestAppContext) {
    let (_dir, handle) = app_window(cx);
    let primary = if cfg!(target_os = "macos") {
        "cmd"
    } else {
        "ctrl"
    };
    assert_eq!(accelerators("commands.show", cx), [format!("{primary}-k")]);
    assert_eq!(
        accelerators("view.toggleSidebar", cx),
        [format!("{primary}-b")]
    );
    // Shortcuts without a menu item are bound too, so any menu can show them.
    assert_eq!(
        accelerators("pane.focusLeft", cx),
        [if cfg!(target_os = "macos") {
            "alt-cmd-left"
        } else {
            "ctrl-alt-left"
        }]
    );
    let kept = other_bindings(cx);
    assert!(kept > 0, "Kit's own bindings exist");

    let overrides: ShortcutOverrides = HashMap::from([
        ("commands.show".to_owned(), Some("Ctrl+Shift+J".to_owned())),
        ("view.toggleSidebar".to_owned(), None),
    ]);
    with_window(handle, cx, |window, cx| {
        commands::set_overrides(overrides, window, cx)
    });
    cx.run_until_parked();
    assert_eq!(accelerators("commands.show", cx), ["ctrl-shift-j"]);
    assert!(accelerators("view.toggleSidebar", cx).is_empty());
    assert_eq!(other_bindings(cx), kept);
}

#[gpui_kit::test]
fn an_accelerator_does_not_run_its_command_a_second_time(cx: &mut TestAppContext) {
    let (_dir, handle) = app_window(cx);
    let ran = Rc::new(Cell::new(0));
    let counter = ran.clone();
    cx.update(|cx| {
        commands::on_command("view.toggleSidebar", cx, move |_, _| {
            counter.set(counter.get() + 1)
        })
    });
    let stroke = if cfg!(target_os = "macos") {
        "cmd-b"
    } else {
        "ctrl-b"
    };
    with_window(handle, cx, |window, cx| window.press(stroke, cx));
    cx.run_until_parked();
    assert_eq!(ran.get(), 1);
}

#[gpui_kit::test]
fn choosing_a_menu_item_runs_an_available_command_unless_a_dialog_is_open(cx: &mut TestAppContext) {
    let (_dir, handle) = app_window(cx);
    let ran = Rc::new(Cell::new(0));
    let counter = ran.clone();
    cx.update(|cx| {
        commands::on_command("view.toggleSidebar", cx, move |_, _| {
            counter.set(counter.get() + 1)
        })
    });
    let choose = |id: &'static str, cx: &mut TestAppContext| {
        cx.dispatch_action(handle, RunCommand { id });
        cx.run_until_parked();
    };
    choose("view.toggleSidebar", cx);
    assert_eq!(ran.get(), 1);
    // No handler, and so unavailable: nothing happens, and nothing panics.
    choose("pane.zoom", cx);
    choose("commands.show", cx);
    assert!(with_window(handle, cx, |window, cx| window.has_active_dialog(cx)));
    choose("view.toggleSidebar", cx);
    assert_eq!(ran.get(), 1, "a dialog owns the keyboard");
}

#[cfg(target_os = "linux")]
#[gpui_kit::test]
fn the_menus_open_from_a_title_bar_button(cx: &mut TestAppContext) {
    let (_dir, handle) = app_window(cx);
    with_window(handle, cx, |window, _| {
        assert_eq!(window.find_by_label("Main menu").role(), Some(Role::Button));
    });
    with_window(handle, cx, |window, cx| window.click("app-menu", cx));
    cx.run_until_parked();
    with_window(handle, cx, |window, _| {
        for name in ["Edit", "View", "Window"] {
            let found = window.try_find_by_label(name);
            assert_eq!(found.and_then(|found| found.role()), Some(Role::MenuItem));
        }
    });
}

#[gpui_kit::test]
fn a_disabled_control_explains_itself_in_a_tooltip(cx: &mut TestAppContext) {
    let (_dir, handle) = app_window(cx);
    let (settings, back) = if cfg!(target_os = "macos") {
        ("Settings (⌘,)", "Back (⌘[)")
    } else {
        ("Settings (Ctrl+,)", "Back (Ctrl+[)")
    };
    with_window(handle, cx, |window, _| {
        for (name, tip) in [
            ("Settings", settings),
            ("Back", back),
            ("No agents waiting", "No agents waiting"),
            ("Close other tabs", "Close other tabs"),
        ] {
            let control = window.find_by_label(name);
            assert_eq!(control.disabled(), Some(true), "{name}");
            assert_eq!(control.description(), Some(tip), "{name}");
        }
    });
}

#[gpui_kit::test]
fn the_sidebar_toggle_says_why_it_is_unavailable(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let path = write_state(dir.path(), None);
    let handle = open_shell(&path, 320., cx);
    with_window(handle, cx, |window, _| {
        let toggle = window.find_by_label("Toggle sidebar");
        assert_eq!(toggle.disabled(), Some(true));
        assert_eq!(
            toggle.description(),
            Some("Widen the window to show the sidebar")
        );
    });
}

#[gpui_kit::test]
fn notices_replace_each_other_and_a_problem_stays(cx: &mut TestAppContext) {
    let (_dir, handle) = app_window(cx);
    let count = |cx: &mut TestAppContext| {
        with_window(handle, cx, |window, cx| window.notifications(cx).len())
    };
    let notify = |message: &str, cx: &mut TestAppContext| {
        let shown = with_window(handle, cx, |window, cx| {
            notices::notify(message, window, cx)
        });
        cx.run_until_parked();
        shown
    };
    assert!(!notify("Live", cx), "routine progress is not a notice");
    assert_eq!(count(cx), 0);

    assert!(notify("Could not save shortcuts: disk full", cx));
    assert_eq!(count(cx), 1);
    cx.executor().advance_clock(Duration::from_secs(30));
    cx.run_until_parked();
    assert_eq!(count(cx), 1, "a problem waits to be dismissed");

    assert!(notify("Copied the path", cx));
    assert_eq!(count(cx), 1, "the new notice replaced the old one");
    cx.executor().advance_clock(Duration::from_secs(30));
    cx.run_until_parked();
    assert_eq!(count(cx), 0, "an information notice goes by itself");
}

/// A row with a context menu: a plain item with a shortcut, a disabled one, a
/// picked option and the destructive one.
struct Row {
    ran: Rc<RefCell<Vec<&'static str>>>,
}

impl Render for Row {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let log = |name: &'static str| {
            let ran = self.ran.clone();
            move |_: &mut Window, _: &mut App| ran.borrow_mut().push(name)
        };
        let entries = Rc::new(vec![
            Entry::from(Item::new("Open", log("open")).command("commands.show")),
            Entry::from(Item::new("Rename…", log("rename")).disabled(true)),
            Entry::Separator,
            Entry::from(Item::new("Sort by name", log("sort")).checked(true)),
            Entry::from(Item::new("Delete…", log("delete")).destructive()),
        ]);
        div().size_full().child(
            div()
                .id("row")
                .w(px(200.))
                .h(px(24.))
                .child("a row")
                .test_support()
                .context_menu(move |menu, _, _| context_menu::fill(menu, &entries)),
        )
    }
}

#[gpui_kit::test]
fn a_context_menu_runs_items_and_skips_disabled_ones(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    boot(&write_state(dir.path(), None), cx);
    let ran = Rc::new(RefCell::new(Vec::new()));
    let view = ran.clone();
    let handle = cx.update(|cx| {
        gpui_kit::open_window(WindowOptions::default(), cx, |_, cx| {
            cx.new(|_| Row { ran: view })
        })
        .expect("open a test window")
        .0
    });
    with_window(handle, cx, |window, cx| window.right_click("row", cx));
    cx.run_until_parked();
    with_window(handle, cx, |window, _| {
        for name in ["Open", "Rename…", "Sort by name"] {
            assert_eq!(
                window.find_by_label(name).role(),
                Some(Role::MenuItem),
                "{name}"
            );
        }
        assert_eq!(window.find_by_label("Sort by name").checked(), Some(true));
    });
    with_window(handle, cx, |window, cx| click_label(window, "Rename…", cx));
    cx.run_until_parked();
    assert!(ran.borrow().is_empty(), "a disabled item does not run");
    with_window(handle, cx, |window, cx| click_label(window, "Open", cx));
    cx.run_until_parked();
    assert_eq!(*ran.borrow(), ["open"]);
}

#[gpui_kit::test]
fn the_window_geometry_round_trips_through_app_state(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let path = write_state(dir.path(), None);
    let handle = open_shell(&path, 1000., cx);
    cx.simulate_window_resize(handle, size(px(900.), px(650.)));
    cx.executor().advance_clock(Duration::from_secs(1));
    cx.run_until_parked();
    let saved = wait_for_geometry(&path);
    // The test window is at 200%: physical pixels, and the scale they were taken at.
    assert_eq!((saved.width, saved.height), (1800, 1300));
    assert!(!saved.maximized);
    assert_eq!(saved.scale_factor_milli, Some(2000));

    // A new run opens where the last one ended.
    let options = cx.update(|cx| app::main_window_options(cx));
    let Some(WindowBounds::Windowed(bounds)) = options.window_bounds else {
        panic!("a saved window opens windowed");
    };
    assert_eq!(
        (bounds.size.width, bounds.size.height),
        (px(900.), px(650.))
    );
}

#[gpui_kit::test]
fn a_saved_geometry_from_tauri_opens_the_window_there(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    // Physical pixels at 200%: 800 by 600 logical.
    let path = write_state(
        dir.path(),
        Some(serde_json::json!({
            "x": 0, "y": 0, "width": 1600, "height": 1200,
            "maximized": true, "scaleFactorMilli": 2000,
        })),
    );
    boot(&path, cx);
    let options = cx.update(|cx| app::main_window_options(cx));
    let Some(WindowBounds::Maximized(bounds)) = options.window_bounds else {
        panic!("a saved maximized window opens maximized");
    };
    assert_eq!(
        (bounds.size.width, bounds.size.height),
        (px(800.), px(600.))
    );
}

#[gpui_kit::test]
fn the_geometry_is_saved_beside_other_saved_state(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let path = write_state(dir.path(), None);
    let handle = open_shell(&path, 1000., cx);
    // Another writer's update lands while the window is resting.
    let overrides: ShortcutOverrides =
        HashMap::from([("commands.show".to_owned(), Some("Ctrl+J".to_owned()))]);
    let saved = overrides.clone();
    with_window(handle, cx, |window, cx| {
        commands::set_overrides(saved, window, cx)
    });
    cx.simulate_window_resize(handle, size(px(700.), px(500.)));
    cx.executor().advance_clock(Duration::from_secs(1));
    cx.run_until_parked();
    wait_for_geometry(&path);
    // Both writes are in the file, whichever went first.
    for _ in 0..500 {
        let state = load_app_state(&AppStateStore::load(path.clone())).unwrap();
        if state.commands.shortcut_overrides == overrides {
            assert!(state.shell.window_geometry.is_some());
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    panic!("the overrides were lost to the geometry write");
}

fn wait_for_geometry(path: &Path) -> WindowGeometry {
    for _ in 0..500 {
        let store = AppStateStore::load(path.to_owned());
        if let Some(geometry) = load_app_state(&store).unwrap().shell.window_geometry {
            return geometry;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    panic!("the geometry did not reach app-state.json");
}

#[gpui_kit::test]
fn a_problem_notice_can_be_dismissed_without_hovering_it(cx: &mut TestAppContext) {
    let (_dir, handle) = app_window(cx);
    with_window(handle, cx, |window, cx| {
        notices::notify("Could not save shortcuts: disk full", window, cx)
    });
    cx.run_until_parked();
    with_window(handle, cx, |window, cx| click_label(window, "Dismiss", cx));
    cx.run_until_parked();
    cx.executor().advance_clock(Duration::from_secs(1));
    cx.run_until_parked();
    let left = with_window(handle, cx, |window, cx| window.notifications(cx).len());
    assert_eq!(left, 0);
}
