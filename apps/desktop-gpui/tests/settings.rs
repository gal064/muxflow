//! Settings through GPUI's test platform: the dialog's controls, that each
//! change is live at once, and that it is saved in the Tauri file format
//! without touching what else the file holds.

use std::{
    path::{Path, PathBuf},
    thread::sleep,
    time::{Duration, Instant},
};

use desktop_core::app_state::{AppStateStore, ThemePreference, load_app_state};
use gpui_kit::{
    AnyWindowHandle, App, AppContext, Bounds, Role, TestAppContext, Window, WindowBounds,
    WindowOptions,
    component::{Theme, WindowExt},
    point, px, size,
    test::{TestQueryExt, TestWindowExt},
};
use muxflow_gpui::{
    app::{self, Core},
    settings::{self, Prefs},
    theme::Appearance,
    ui::shell::Shell,
};
use serde_json::{Value, json};

fn write_state(dir: &Path, shell: Value) -> PathBuf {
    let path = dir.join("app-state.json");
    let state = json!({
        "schemaVersion": 1,
        "appTabs": [],
        "workspaceUi": [],
        "shell": shell,
        "commands": { "shortcutOverrides": { "commands.show": "Ctrl+Shift+J" } },
    });
    std::fs::write(&path, serde_json::to_vec(&state).unwrap()).unwrap();
    path
}

fn open_app(state: &Path, cx: &mut TestAppContext) -> AnyWindowHandle {
    cx.update(|cx| {
        gpui_kit::init(cx);
        app::init(Core::at(state), cx);
        cx.set_reduce_motion(true);
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds {
                origin: point(px(0.), px(0.)),
                size: size(px(1280.), px(900.)),
            })),
            ..Default::default()
        };
        gpui_kit::open_window(options, cx, Shell::open)
            .expect("open a test window")
            .0
    })
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

fn press(handle: AnyWindowHandle, stroke: &str, cx: &mut TestAppContext) {
    with_window(handle, cx, |window, cx| window.press(stroke, cx));
    cx.run_until_parked();
}

fn click(handle: AnyWindowHandle, id: &'static str, cx: &mut TestAppContext) {
    with_window(handle, cx, |window, cx| window.click(id, cx));
    cx.run_until_parked();
}

/// Clicks the tab with this name. The app has a "Workspace" region too, so the
/// match is the one inside the tab bar.
fn click_tab(handle: AnyWindowHandle, label: &str, cx: &mut TestAppContext) {
    with_window(handle, cx, |window, cx| {
        let id = window
            .find_all_by_label(label)
            .into_iter()
            .find(|tab| tab.path().contains(&"settings-tabs".into()))
            .unwrap_or_else(|| panic!("no tab {label}"))
            .path()
            .last()
            .cloned()
            .expect("an element with an id");
        window.within("settings-tabs").click(id, cx);
    });
    cx.run_until_parked();
}

/// Picks the radio at `index` in a group.
fn pick(handle: AnyWindowHandle, group: &'static str, index: usize, cx: &mut TestAppContext) {
    with_window(handle, cx, |window, cx| {
        window.within(group).click(index, cx);
    });
    cx.run_until_parked();
}

fn type_into(handle: AnyWindowHandle, id: &'static str, text: &str, cx: &mut TestAppContext) {
    click(handle, id, cx);
    press(handle, "secondary-a", cx);
    if text.is_empty() {
        press(handle, "backspace", cx);
    } else {
        with_window(handle, cx, |window, cx| window.input(text, cx));
        cx.run_until_parked();
    }
    press(handle, "enter", cx);
}

fn prefs(cx: &mut TestAppContext) -> Prefs {
    cx.read(|cx| settings::get(cx).clone())
}

fn open_settings(handle: AnyWindowHandle, tab: Option<&str>, cx: &mut TestAppContext) {
    press(handle, "secondary-,", cx);
    assert!(
        with_window(handle, cx, |window, cx| window.has_active_dialog(cx)),
        "the shortcut opens Settings"
    );
    if let Some(tab) = tab {
        click_tab(handle, tab, cx);
    }
}

/// Runs the app until the saved file satisfies `check`. The save runs on the
/// backend's blocking pool, which the deterministic scheduler cannot reach.
fn saved(cx: &mut TestAppContext, path: &Path, check: impl Fn(&Value) -> bool) -> Value {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        cx.run_until_parked();
        let value: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        if check(&value) {
            return value;
        }
        assert!(Instant::now() < deadline, "not saved: {value}");
        sleep(Duration::from_millis(10));
    }
}

#[gpui_kit::test]
fn the_shortcut_and_the_gear_open_settings_with_its_six_tabs(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let state = write_state(dir.path(), json!({}));
    let handle = open_app(&state, cx);

    click(handle, "title-settings", cx);
    with_window(handle, cx, |window, cx| {
        assert!(window.has_active_dialog(cx));
        for tab in [
            "Connection",
            "Workspace",
            "Terminal",
            "Appearance",
            "Sounds",
            "Accessibility",
        ] {
            assert!(
                window
                    .find_all_by_label(tab)
                    .iter()
                    .any(|found| found.path().contains(&"settings-tabs".into())),
                "{tab}"
            );
        }
    });
    press(handle, "escape", cx);
    assert!(!with_window(handle, cx, |window, cx| window.has_active_dialog(cx)));
    open_settings(handle, None, cx);
}

/// Every checkbox and radio: the tab it is on, its id, how to read it, and its
/// label, which is Tauri's copy.
#[allow(clippy::type_complexity)]
const TOGGLES: &[(&str, &str, &str, fn(&Prefs) -> bool)] = &[
    (
        "Workspace",
        "settings-compact-workspaces",
        "Compact workspace list",
        |p| p.compact_workspaces,
    ),
    (
        "Workspace",
        "settings-extension-automations",
        "Show Automations",
        |p| p.panel_extension_enabled("automations"),
    ),
    (
        "Terminal",
        "settings-copy-on-select",
        "Copy on select",
        |p| p.copy_on_select,
    ),
    (
        "Terminal",
        "settings-clean-wrapped",
        "Clean wrapped commands when copying",
        |p| p.clean_wrapped_commands,
    ),
    (
        "Terminal",
        "settings-application-clipboard",
        "Allow terminal apps to copy",
        |p| p.terminal_application_clipboard,
    ),
    (
        "Terminal",
        "settings-focus-follows-mouse",
        "Focus pane on hover",
        |p| p.focus_follows_mouse,
    ),
    (
        "Appearance",
        "settings-tabs-in-title-bar",
        "Show tabs in the title bar",
        |p| p.tabs_in_title_bar,
    ),
    (
        "Sounds",
        "settings-sounds-blocked",
        "Cue when an agent is blocked",
        |p| p.agent_sounds.blocked != desktop_core::app_state::AgentSoundMode::None,
    ),
    (
        "Sounds",
        "settings-sounds-completed",
        "Cue when an agent finishes",
        |p| p.agent_sounds.completed != desktop_core::app_state::AgentSoundMode::None,
    ),
    // Last of the sounds: while it is off the two cues are disabled.
    (
        "Sounds",
        "settings-sounds-enabled",
        "Play agent cues",
        |p| p.agent_sounds.enabled,
    ),
    (
        "Accessibility",
        "settings-agent-state-glyphs",
        "Shapes in agent state dots",
        |p| p.agent_state_glyphs,
    ),
];

#[gpui_kit::test]
fn every_checkbox_is_named_live_and_saved(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    // The layout and a flag Tauri still stores must survive every save.
    let state = write_state(
        dir.path(),
        json!({ "sidebarWidth": 321.0, "panelOpen": true, "terminalScreenReader": true }),
    );
    let handle = open_app(&state, cx);
    open_settings(handle, None, cx);
    for (tab, id, label, read) in TOGGLES {
        click_tab(handle, tab, cx);
        let before = read(&prefs(cx));
        with_window(handle, cx, |window, _| {
            let found = window.find_by_label(label);
            assert_eq!(found.role(), Some(Role::CheckBox), "{label}");
            assert_eq!(found.checked(), Some(before), "{label}");
        });
        with_window(handle, cx, |window, cx| window.click(*id, cx));
        cx.run_until_parked();
        assert_eq!(read(&prefs(cx)), !before, "{label} is live");
        with_window(handle, cx, |window, _| {
            assert_eq!(
                window.find_by_label(label).checked(),
                Some(!before),
                "{label}"
            );
        });
    }
    let value = saved(cx, &state, |value| {
        value["shell"]["agentStateGlyphs"] == json!(true)
            && value["shell"]["copyOnSelect"] == json!(false)
    });
    let shell = &value["shell"];
    assert_eq!(shell["focusFollowsMouse"], json!(true));
    assert_eq!(shell["tabsInTitleBar"], json!(false));
    assert_eq!(shell["compactWorkspaces"], json!(false));
    assert_eq!(shell["cleanWrappedCommands"], json!(false));
    assert_eq!(shell["terminalApplicationClipboard"], json!(false));
    assert_eq!(shell["enabledPanelExtensions"], json!([]));
    assert_eq!(shell["agentSounds"]["enabled"], json!(false));
    // What else the file holds is untouched.
    assert_eq!(shell["sidebarWidth"], json!(321.0));
    assert_eq!(shell["panelOpen"], json!(true));
    assert_eq!(shell["terminalScreenReader"], json!(true));
    assert_eq!(
        value["commands"]["shortcutOverrides"]["commands.show"],
        json!("Ctrl+Shift+J")
    );
}

#[gpui_kit::test]
fn a_dependent_sound_cue_is_off_while_the_sounds_are_off(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let state = write_state(dir.path(), json!({}));
    let handle = open_app(&state, cx);
    open_settings(handle, Some("Sounds"), cx);
    let disabled = |cx: &mut TestAppContext| {
        with_window(handle, cx, |window, _| {
            window
                .find_by_label("Cue when an agent is blocked")
                .disabled()
        })
    };
    assert_ne!(disabled(cx), Some(true));
    click(handle, "settings-sounds-enabled", cx);
    assert_eq!(disabled(cx), Some(true));
}

#[gpui_kit::test]
fn the_theme_and_the_interface_size_apply_to_the_window_at_once(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let state = write_state(dir.path(), json!({ "theme": "dark", "uiScale": 1.0 }));
    let handle = open_app(&state, cx);
    open_settings(handle, Some("Appearance"), cx);

    pick(handle, "settings-theme", 1, cx);
    cx.read(|cx| {
        let appearance = cx.global::<Appearance>();
        assert_eq!(appearance.preference, ThemePreference::Light);
        assert!(!appearance.is_dark);
        assert_eq!(
            Theme::global(cx).background,
            appearance.palette.chrome_bg.into()
        );
    });
    pick(handle, "settings-ui-scale", 3, cx);
    cx.read(|cx| {
        assert_eq!(cx.global::<Appearance>().scale, 1.2);
        assert_eq!(Theme::global(cx).font_size, px(19.2));
    });
    let value = saved(cx, &state, |value| value["shell"]["uiScale"] == json!(1.2));
    assert_eq!(value["shell"]["theme"], json!("light"));
}

#[gpui_kit::test]
fn markdown_choices_are_saved_by_their_tauri_names(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let state = write_state(dir.path(), json!({}));
    let handle = open_app(&state, cx);
    open_settings(handle, Some("Appearance"), cx);
    pick(handle, "settings-markdown-appearance", 2, cx);
    click_tab(handle, "Workspace", cx);
    pick(handle, "settings-markdown-view", 2, cx);
    let value = saved(cx, &state, |value| {
        value["shell"]["defaultMarkdownView"] == json!("preview")
    });
    assert_eq!(value["shell"]["markdownAppearance"], json!("dark"));
}

#[gpui_kit::test]
fn the_font_size_commits_when_typed_and_stays_within_its_bounds(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let state = write_state(dir.path(), json!({ "terminalFontSize": 14 }));
    let handle = open_app(&state, cx);
    open_settings(handle, Some("Terminal"), cx);
    assert_eq!(prefs(cx).terminal_font_size, 14);

    type_into(handle, "settings-terminal-font-size", "18", cx);
    assert_eq!(prefs(cx).terminal_font_size, 18);
    saved(cx, &state, |value| {
        value["shell"]["terminalFontSize"] == json!(18)
    });

    type_into(handle, "settings-terminal-font-size", "99", cx);
    assert_eq!(prefs(cx).terminal_font_size, 20, "clamped once, on commit");
    // An empty field is not a size: the one in force stays.
    type_into(handle, "settings-terminal-font-size", "", cx);
    assert_eq!(prefs(cx).terminal_font_size, 20);
    with_window(handle, cx, |window, _| {
        assert_eq!(
            window.find_by_label("Terminal font size").value(),
            Some("20")
        );
    });
}

/// A click on a checkbox or a button does not move focus out of a field, so
/// what was typed must count when the dialog closes or the tab changes.
#[gpui_kit::test]
fn typed_text_counts_when_the_tab_changes_or_the_dialog_closes(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let state = write_state(dir.path(), json!({ "terminalFontSize": 14 }));
    let handle = open_app(&state, cx);
    open_settings(handle, Some("Terminal"), cx);
    let type_without_enter = |text: &str, cx: &mut TestAppContext| {
        click(handle, "settings-terminal-font-size", cx);
        press(handle, "secondary-a", cx);
        with_window(handle, cx, |window, cx| window.input(text, cx));
        cx.run_until_parked();
    };

    type_without_enter("16", cx);
    assert_eq!(prefs(cx).terminal_font_size, 14, "half-typed is not a save");
    click_tab(handle, "Appearance", cx);
    assert_eq!(prefs(cx).terminal_font_size, 16, "leaving the tab commits");

    click_tab(handle, "Terminal", cx);
    type_without_enter("17", cx);
    click(handle, "settings-done", cx);
    assert_eq!(prefs(cx).terminal_font_size, 17, "Done commits");

    open_settings(handle, Some("Terminal"), cx);
    type_without_enter("18", cx);
    press(handle, "escape", cx);
    assert_eq!(prefs(cx).terminal_font_size, 18, "Escape commits");
}

#[gpui_kit::test]
fn workspace_defaults_are_per_host_and_an_empty_field_clears_them(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let state = write_state(dir.path(), json!({}));
    let handle = open_app(&state, cx);
    open_settings(handle, Some("Workspace"), cx);

    type_into(handle, "settings-start-directory", "  /work  ", cx);
    type_into(handle, "settings-startup-command", "htop", cx);
    let defaults = prefs(cx).defaults_for("local");
    assert_eq!(defaults.directory.as_deref(), Some("/work"));
    assert_eq!(defaults.startup_command.as_deref(), Some("htop"));
    let value = saved(cx, &state, |value| {
        value["workspaceDefaults"]["local"]["startupCommand"] == json!("htop")
    });
    assert_eq!(
        value["workspaceDefaults"]["local"]["directory"],
        json!("/work")
    );

    type_into(handle, "settings-start-directory", "", cx);
    type_into(handle, "settings-startup-command", "", cx);
    saved(cx, &state, |value| value["workspaceDefaults"] == json!({}));
}

#[gpui_kit::test]
fn a_change_is_what_the_next_launch_reads(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let state = write_state(dir.path(), json!({}));
    let handle = open_app(&state, cx);
    open_settings(handle, Some("Terminal"), cx);
    click(handle, "settings-focus-follows-mouse", cx);
    click(handle, "settings-copy-on-select", cx);
    let after = prefs(cx);
    saved(cx, &state, |value| {
        value["shell"]["copyOnSelect"] == json!(false)
    });
    let relaunched = Prefs::from_state(&load_app_state(&AppStateStore::load(state)).unwrap());
    assert_eq!(relaunched, after);
}

#[gpui_kit::test]
fn the_notification_check_says_so_when_nothing_can_deliver(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let state = write_state(dir.path(), json!({}));
    let handle = open_app(&state, cx);
    open_settings(handle, Some("Sounds"), cx);
    with_window(handle, cx, |window, _| {
        assert_eq!(
            window.find_by_label("Send test notification").disabled(),
            Some(true)
        );
        assert_eq!(
            window.find_by_label("Agent sound volume").role(),
            Some(Role::Slider)
        );
    });
}

#[gpui_kit::test]
fn done_closes_every_tab_but_connection_which_has_connect(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let state = write_state(dir.path(), json!({}));
    let handle = open_app(&state, cx);
    open_settings(handle, Some("Terminal"), cx);
    with_window(handle, cx, |window, _| {
        assert!(window.try_find_by_label("Done").is_some());
        assert!(window.try_find_by_label("Connect").is_none());
    });
    click(handle, "settings-done", cx);
    assert!(!with_window(handle, cx, |window, cx| window.has_active_dialog(cx)));
    open_settings(handle, None, cx);
    with_window(handle, cx, |window, _| {
        assert!(window.try_find_by_label("Connect").is_some());
    });
}
