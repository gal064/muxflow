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
    AnyWindowHandle, App, AppContext, Bounds, Role, ScrollDelta, TestAppContext, Window,
    WindowBounds, WindowOptions,
    component::{Theme, WindowExt},
    point, px, size,
    test::{TestQueryExt, TestWindowExt},
};
use muxflow_gpui::{
    app::{self, Core},
    commands::{self, Commands, keymap::ShortcutOverrides},
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
    reveal(handle, id, cx);
    with_window(handle, cx, |window, cx| window.click(id, cx));
    cx.run_until_parked();
}

/// Scrolls what holds `id`, the shortcut list or else the Settings body,
/// until `id` is in view, as a person would before clicking it.
fn reveal(handle: AnyWindowHandle, id: &'static str, cx: &mut TestAppContext) {
    for _ in 0..80 {
        let below = with_window(handle, cx, |window, _| {
            let target = window.find(id);
            let scroller = scroller(&target);
            (!target.visible()).then(|| {
                (
                    target.bounds().origin.y > window.find(scroller).bounds().origin.y,
                    scroller,
                )
            })
        });
        let Some((below, scroller)) = below else {
            return;
        };
        let step = if below { px(-80.) } else { px(80.) };
        with_window(handle, cx, |window, cx| {
            window.scroll(scroller, ScrollDelta::Pixels(point(px(0.), step)), cx)
        });
        cx.run_until_parked();
    }
    panic!("{id} never scrolled into view");
}

fn scroller(target: &gpui_kit::test::ElementSnapshot) -> &'static str {
    if target.path().contains(&"settings-shortcuts".into()) {
        "settings-shortcuts"
    } else {
        "settings-body"
    }
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
fn the_shortcut_and_the_gear_open_settings_with_its_eight_tabs(cx: &mut TestAppContext) {
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
            "Agents",
            "Keyboard",
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
    // A saved Live is kept while other settings are written, and Live is
    // offered beside Source, Preview and Split.
    let state = write_state(dir.path(), json!({ "defaultMarkdownView": "live" }));
    let handle = open_app(&state, cx);
    open_settings(handle, Some("Appearance"), cx);
    pick(handle, "settings-markdown-appearance", 2, cx);
    let value = saved(cx, &state, |value| {
        value["shell"]["markdownAppearance"] == json!("dark")
    });
    assert_eq!(value["shell"]["defaultMarkdownView"], json!("live"));
    click_tab(handle, "Workspace", cx);
    pick(handle, "settings-markdown-view", 2, cx);
    saved(cx, &state, |value| {
        value["shell"]["defaultMarkdownView"] == json!("preview")
    });
    pick(handle, "settings-markdown-view", 1, cx);
    saved(cx, &state, |value| {
        value["shell"]["defaultMarkdownView"] == json!("live")
    });
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
    type_into(handle, "settings-agent-command", "codex", cx);
    let defaults = prefs(cx).defaults_for("local");
    assert_eq!(defaults.directory.as_deref(), Some("/work"));
    assert_eq!(defaults.startup_command.as_deref(), Some("htop"));
    assert_eq!(defaults.agent_command.as_deref(), Some("codex"));
    let value = saved(cx, &state, |value| {
        value["workspaceDefaults"]["local"]["startupCommand"] == json!("htop")
    });
    assert_eq!(
        value["workspaceDefaults"]["local"]["directory"],
        json!("/work")
    );

    assert_eq!(
        value["workspaceDefaults"]["local"]["agentCommand"],
        json!("codex")
    );

    type_into(handle, "settings-start-directory", "", cx);
    type_into(handle, "settings-startup-command", "", cx);
    type_into(handle, "settings-agent-command", "", cx);
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

#[test]
fn the_volume_keys_move_it_as_a_range_input_does() {
    let at = |key: &str, volume: f32| {
        settings::dialog::volume_for_key(key, volume).map(|volume| (volume * 100.).round() / 100.)
    };
    assert_eq!(at("right", 0.5), Some(0.55));
    assert_eq!(at("up", 0.5), Some(0.55));
    assert_eq!(at("left", 0.5), Some(0.45));
    assert_eq!(at("down", 0.5), Some(0.45));
    assert_eq!(at("pageup", 0.5), Some(0.6));
    assert_eq!(at("pagedown", 0.05), Some(0.));
    assert_eq!(at("home", 0.7), Some(0.));
    assert_eq!(at("end", 0.7), Some(1.));
    assert_eq!(at("right", 1.), Some(1.));
    assert_eq!(at("left", 0.), Some(0.));
    // Off the grid, as a saved file can be: the step lands back on it.
    assert_eq!(at("right", 0.33), Some(0.4));
    assert_eq!(at("a", 0.5), None);
}

/// Tab reaches the volume, every key that moves it saves it, and Settings opened
/// again shows the saved value.
#[gpui_kit::test]
fn the_volume_is_moved_and_saved_from_the_keyboard(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let state = write_state(
        dir.path(),
        json!({
            "agentSounds": { "enabled": true, "blocked": "subtle", "completed": "subtle", "volume": 0.5 }
        }),
    );
    let handle = open_app(&state, cx);
    open_settings(handle, Some("Sounds"), cx);
    let focused = |cx: &mut TestAppContext| {
        with_window(handle, cx, |window, _| {
            window.find_by_label("Agent sound volume").focused() == Some(true)
        })
    };
    for _ in 0..10 {
        if focused(cx) {
            break;
        }
        press(handle, "tab", cx);
    }
    assert!(focused(cx), "Tab never reached the volume");

    let volume = |cx: &mut TestAppContext| prefs(cx).agent_sounds.volume;
    for (key, expected) in [
        ("right", 0.55),
        ("up", 0.6),
        ("left", 0.55),
        ("down", 0.5),
        ("pageup", 0.6),
        ("pagedown", 0.5),
        ("home", 0.),
        ("end", 1.),
        ("left", 0.95),
    ] {
        press(handle, key, cx);
        assert_eq!(volume(cx), expected, "after {key}");
    }
    saved(cx, &state, |value| {
        value["shell"]["agentSounds"]["volume"] == json!(0.95)
    });

    press(handle, "escape", cx);
    open_settings(handle, Some("Sounds"), cx);
    press(handle, "tab", cx);
    for _ in 0..10 {
        if focused(cx) {
            break;
        }
        press(handle, "tab", cx);
    }
    assert!(focused(cx), "Tab never reached the reopened volume");
    press(handle, "left", cx);
    assert_eq!(volume(cx), 0.9, "reopened at the saved 0.95");
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

#[gpui_kit::test]
fn a_reset_of_saved_state_makes_its_defaults_live(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let state = write_state(
        dir.path(),
        json!({ "theme": "dark", "copyOnSelect": false }),
    );
    let handle = open_app(&state, cx);
    let palette_opens = |cx: &mut TestAppContext| {
        press(handle, "secondary-shift-p", cx);
        let open = with_window(handle, cx, |window, cx| window.has_active_dialog(cx));
        if open {
            press(handle, "escape", cx);
        }
        open
    };
    assert!(!palette_opens(cx), "the saved override moved it");
    let defaults = desktop_core::app_state::PersistedAppState::default();
    cx.update(|cx| {
        settings::reload(&defaults, cx);
        muxflow_gpui::commands::reload_overrides(&defaults, cx);
    });
    assert_eq!(
        cx.read(|cx| settings::get(cx).clone()),
        Prefs::from_state(&defaults)
    );
    assert!(palette_opens(cx));
}

/// What a QA driver does: finds each section by role and name, inside the
/// named section list, and presses it. GPUI performs an accessibility press on
/// a node with no handler of its own as a click at the node's centre; the test
/// platform has no adapter to send one, so the click is sent here.
#[gpui_kit::test]
fn each_settings_section_is_found_by_role_and_name_and_pressed(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let state = write_state(dir.path(), json!({}));
    let handle = open_app(&state, cx);
    open_settings(handle, None, cx);
    let sections = [
        "Connection",
        "Workspace",
        "Terminal",
        "Agents",
        "Keyboard",
        "Appearance",
        "Sounds",
        "Accessibility",
    ];
    for name in sections.iter().rev() {
        let centre = with_window(handle, cx, |window, _| {
            let list = window.find_by_label("Settings sections");
            assert_eq!(list.role(), Some(Role::Group));
            let tab = window
                .find_all_by_role(Role::Tab)
                .into_iter()
                .find(|tab| tab.label() == Some(*name) && tab.path().starts_with(list.path()))
                .unwrap_or_else(|| panic!("no section {name}"));
            tab.bounds().center()
        });
        let mut visual = gpui_kit::VisualTestContext::from_window(handle, cx);
        visual.simulate_click(centre, gpui_kit::Modifiers::none());
        visual.run_until_parked();
        let selected = with_window(handle, cx, |window, _| {
            window
                .find_all_by_role(Role::Tab)
                .into_iter()
                .find(|tab| tab.label() == Some(*name))
                .and_then(|tab| tab.selected())
        });
        assert_eq!(selected, Some(true), "{name}");
    }
}

#[gpui_kit::test]
fn tab_reaches_the_sections_as_one_stop_and_the_arrows_move_between_them(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let state = write_state(dir.path(), json!({}));
    let handle = open_app(&state, cx);
    open_settings(handle, Some("Accessibility"), cx);
    let selected = |cx: &mut TestAppContext| {
        with_window(handle, cx, |window, _| {
            window
                .elements()
                .into_iter()
                .filter(|tab| tab.path().contains(&"settings-tabs".into()))
                .find(|tab| tab.role() == Some(Role::Tab) && tab.selected() == Some(true))
                .and_then(|tab| tab.label().map(str::to_owned))
        })
    };
    // Right does nothing on this section's checkboxes; on a section it moves.
    let reached = (0..20).any(|_| {
        press(handle, "tab", cx);
        press(handle, "right", cx);
        selected(cx).as_deref() != Some("Accessibility")
    });
    assert!(reached, "Tab reaches a section");
    assert_eq!(selected(cx).as_deref(), Some("Connection"), "Right wraps");
    press(handle, "left", cx);
    assert_eq!(selected(cx).as_deref(), Some("Accessibility"), "Left wraps");
    press(handle, "left", cx);
    assert_eq!(selected(cx).as_deref(), Some("Sounds"));
    // One stop, as Tauri's tablist: Tab leaves the sections, Shift+Tab comes
    // back to the one shown.
    press(handle, "tab", cx);
    press(handle, "right", cx);
    assert_eq!(selected(cx).as_deref(), Some("Sounds"));
    press(handle, "shift-tab", cx);
    press(handle, "right", cx);
    assert_ne!(selected(cx).as_deref(), Some("Sounds"));
}

#[gpui_kit::test]
fn home_and_end_select_the_section_they_move_to(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let state = write_state(dir.path(), json!({}));
    let handle = open_app(&state, cx);
    open_settings(handle, Some("Accessibility"), cx);
    // The section that is shown, and the one the keyboard is on.
    let sections = |cx: &mut TestAppContext| {
        with_window(handle, cx, |window, _| {
            let tabs: Vec<_> = window
                .find_all_by_role(Role::Tab)
                .into_iter()
                .filter(|tab| tab.path().contains(&"settings-tabs".into()))
                .collect();
            let label = |want: fn(&gpui_kit::test::ElementSnapshot) -> bool| {
                tabs.iter()
                    .find(|tab| want(tab))
                    .and_then(|tab| tab.label().map(str::to_owned))
            };
            (
                label(|tab| tab.selected() == Some(true)),
                label(|tab| tab.focused() == Some(true)),
            )
        })
    };
    let reached = (0..20).any(|_| {
        press(handle, "tab", cx);
        sections(cx).1.is_some()
    });
    assert!(reached, "Tab reaches a section");
    press(handle, "home", cx);
    let first = Some("Connection".to_owned());
    assert_eq!(sections(cx), (first.clone(), first));
    press(handle, "end", cx);
    let last = Some("Accessibility".to_owned());
    assert_eq!(sections(cx), (last.clone(), last));
}

// Settings › Keyboard.

/// The primary modifier, in stored form.
const PRIMARY: &str = if cfg!(target_os = "macos") {
    "Meta"
} else {
    "Ctrl"
};

fn overrides(cx: &mut TestAppContext) -> ShortcutOverrides {
    cx.read(|cx| cx.global::<Commands>().overrides().clone())
}

/// Settings open on Keyboard, as the palette's command opens it.
fn open_keyboard(cx: &mut TestAppContext) -> (tempfile::TempDir, PathBuf, AnyWindowHandle) {
    let dir = tempfile::tempdir().unwrap();
    let state = write_state(dir.path(), json!({}));
    let handle = open_app(&state, cx);
    with_window(handle, cx, |window, cx| {
        commands::run("shortcuts.configure", window, cx)
    });
    cx.run_until_parked();
    (dir, state, handle)
}

/// Clicks a row's keys and presses `stroke` into the recorder.
fn record(handle: AnyWindowHandle, id: &str, stroke: &str, cx: &mut TestAppContext) {
    let keys: &'static str = Box::leak(format!("shortcut-keys-{id}").into_boxed_str());
    click(handle, keys, cx);
    with_window(handle, cx, |window, _| {
        assert_eq!(window.find("shortcut-recorder").focused(), Some(true));
    });
    press(handle, stroke, cx);
}

fn alerts(handle: AnyWindowHandle, cx: &mut TestAppContext) -> Vec<String> {
    with_window(handle, cx, |window, _| {
        window
            .find_all_by_role(Role::Alert)
            .into_iter()
            .filter_map(|alert| alert.label().map(str::to_owned))
            .collect()
    })
}

fn dialog_open(handle: AnyWindowHandle, cx: &mut TestAppContext) -> bool {
    with_window(handle, cx, |window, cx| window.has_active_dialog(cx))
}

#[gpui_kit::test]
fn configure_shortcuts_opens_settings_on_keyboard(cx: &mut TestAppContext) {
    let (_dir, _, handle) = open_keyboard(cx);
    assert!(dialog_open(handle, cx));
    with_window(handle, cx, |window, _| {
        let keyboard = window
            .find_all_by_role(Role::Tab)
            .into_iter()
            .find(|tab| tab.label() == Some("Keyboard"))
            .expect("a Keyboard section");
        assert_eq!(keyboard.selected(), Some(true));
        assert!(window.try_find_by_label("Search shortcuts").is_some());
    });
}

#[gpui_kit::test]
fn a_recorded_chord_is_saved_and_live_at_once(cx: &mut TestAppContext) {
    let (_dir, state, handle) = open_keyboard(cx);
    record(handle, "session.new", "secondary-j", cx);
    let chord = format!("{PRIMARY}+J");
    assert_eq!(
        overrides(cx),
        ShortcutOverrides::from([
            ("commands.show".to_owned(), Some("Ctrl+Shift+J".to_owned())),
            ("session.new".to_owned(), Some(chord.clone())),
        ])
    );
    saved(cx, &state, |value| {
        value["commands"]["shortcutOverrides"]["session.new"] == json!(chord)
    });
    assert!(dialog_open(handle, cx), "recording never runs a command");
}

#[gpui_kit::test]
fn a_chord_without_a_command_modifier_is_refused_on_its_row(cx: &mut TestAppContext) {
    let (_dir, _, handle) = open_keyboard(cx);
    let before = overrides(cx);
    record(handle, "session.new", "k", cx);
    assert_eq!(
        alerts(handle, cx),
        [if cfg!(target_os = "macos") {
            "Shortcuts need ⌘, ⌥ or ⌃."
        } else {
            "Shortcuts need Ctrl, Alt or Super."
        }]
    );
    assert_eq!(overrides(cx), before);
    // Still recording: the next chord is taken.
    press(handle, "secondary-j", cx);
    assert_eq!(
        overrides(cx).get("session.new"),
        Some(&Some(format!("{PRIMARY}+J")))
    );
    assert!(alerts(handle, cx).is_empty());
}

#[gpui_kit::test]
fn a_taken_chord_asks_and_swap_replace_or_cancel_resolve_it(cx: &mut TestAppContext) {
    let (_dir, _, handle) = open_keyboard(cx);
    let before = overrides(cx);
    // New workspace onto New terminal tab's chord.
    record(handle, "session.new", "secondary-t", cx);
    let (taken, own) = if cfg!(target_os = "macos") {
        ("⌘T", "⌘N")
    } else {
        ("Ctrl+T", "Ctrl+N")
    };
    assert_eq!(
        alerts(handle, cx),
        [format!(
            "{taken} is used by New terminal tab. Swap gives it {own}."
        )]
    );
    assert_eq!(overrides(cx), before, "nothing is written before a choice");
    click(handle, "shortcut-swap", cx);
    let after = overrides(cx);
    assert_eq!(
        after.get("session.new"),
        Some(&Some(format!("{PRIMARY}+T")))
    );
    assert_eq!(after.get("window.new"), Some(&Some(format!("{PRIMARY}+N"))));
    assert!(alerts(handle, cx).is_empty());

    // Toggle right panel onto Toggle sidebar's chord, which loses it.
    record(handle, "view.togglePanel", "secondary-b", cx);
    click(handle, "shortcut-replace", cx);
    let after = overrides(cx);
    assert_eq!(
        after.get("view.togglePanel"),
        Some(&Some(format!("{PRIMARY}+B")))
    );
    assert_eq!(after.get("view.toggleSidebar"), Some(&None));

    // Cancel writes nothing.
    let before = overrides(cx);
    record(handle, "session.new", "secondary-p", cx);
    click(handle, "shortcut-cancel", cx);
    assert_eq!(overrides(cx), before);
    assert!(alerts(handle, cx).is_empty());
}

#[gpui_kit::test]
fn a_rows_buttons_hand_the_keyboard_back_to_its_keys_or_the_search(cx: &mut TestAppContext) {
    let (_dir, _, handle) = open_keyboard(cx);
    let focused = |cx: &mut TestAppContext, id: &'static str| {
        with_window(handle, cx, |window, _| {
            window.find(id).focused() == Some(true)
        })
    };
    record(handle, "session.new", "secondary-t", cx);
    click(handle, "shortcut-cancel", cx);
    assert!(focused(cx, "shortcut-keys-session.new"), "after Cancel");
    record(handle, "session.new", "secondary-t", cx);
    click(handle, "shortcut-swap", cx);
    assert!(focused(cx, "shortcut-keys-session.new"), "after Swap");
    // Reset leaves the Changed list: the search takes the keyboard.
    click(handle, "settings-shortcuts-changed", cx);
    click(handle, "shortcut-reset-commands.show", cx);
    let search = with_window(handle, cx, |window, _| {
        window.find_by_label("Search shortcuts").focused()
    });
    assert_eq!(search, Some(true), "after a reset the filter hides");
}

#[gpui_kit::test]
fn backspace_removes_a_binding(cx: &mut TestAppContext) {
    let (_dir, state, handle) = open_keyboard(cx);
    record(handle, "window.new", "backspace", cx);
    assert_eq!(overrides(cx).get("window.new"), Some(&None));
    saved(cx, &state, |value| {
        value["commands"]["shortcutOverrides"]
            .as_object()
            .is_some_and(|map| map.get("window.new") == Some(&Value::Null))
    });
}

#[gpui_kit::test]
fn escape_while_recording_ends_the_recording_and_keeps_settings_open(cx: &mut TestAppContext) {
    let (_dir, _, handle) = open_keyboard(cx);
    let before = overrides(cx);
    record(handle, "session.new", "escape", cx);
    assert!(dialog_open(handle, cx), "Settings stays open");
    with_window(handle, cx, |window, _| {
        assert!(
            window
                .try_find_by_label("Press the new shortcut for New workspace")
                .is_none()
        );
    });
    // The keyboard is back on the row's keys.
    with_window(handle, cx, |window, _| {
        assert_eq!(
            window.find("shortcut-keys-session.new").focused(),
            Some(true)
        );
    });
    assert_eq!(overrides(cx), before);
    press(handle, "escape", cx);
    assert!(!dialog_open(handle, cx), "a second Escape closes Settings");
}

#[gpui_kit::test]
fn a_row_resets_and_restore_all_brings_back_every_default(cx: &mut TestAppContext) {
    let (_dir, state, handle) = open_keyboard(cx);
    // The saved state moved Show command palette off its default.
    click(handle, "shortcut-reset-commands.show", cx);
    assert_eq!(overrides(cx), ShortcutOverrides::new());

    record(handle, "session.new", "secondary-j", cx);
    record(handle, "window.new", "backspace", cx);
    assert_eq!(overrides(cx).len(), 2);
    click(handle, "settings-shortcuts-restore", cx);
    assert_eq!(overrides(cx), ShortcutOverrides::new());
    saved(cx, &state, |value| {
        value["commands"]["shortcutOverrides"] == json!({})
    });
}

#[gpui_kit::test]
fn search_and_the_changed_filter_narrow_the_list(cx: &mut TestAppContext) {
    let (_dir, _, handle) = open_keyboard(cx);
    let shown = |cx: &mut TestAppContext, title: &str| {
        let prefix = format!("Shortcut for {title}:");
        with_window(handle, cx, |window, _| {
            window.elements().iter().any(|element| {
                element
                    .label()
                    .is_some_and(|label| label.starts_with(&prefix))
            })
        })
    };
    assert!(shown(cx, "Toggle pane zoom") && shown(cx, "New workspace"));
    click(handle, "settings-shortcuts-search", cx);
    with_window(handle, cx, |window, cx| window.input("zoom", cx));
    cx.run_until_parked();
    assert!(shown(cx, "Toggle pane zoom"));
    assert!(!shown(cx, "New workspace"));

    press(handle, "secondary-a", cx);
    press(handle, "backspace", cx);
    click(handle, "settings-shortcuts-changed", cx);
    assert!(
        shown(cx, "Show command palette"),
        "changed by the saved state"
    );
    assert!(!shown(cx, "New workspace"));
}

#[gpui_kit::test]
fn the_number_keys_choice_is_saved(cx: &mut TestAppContext) {
    let (_dir, state, handle) = open_keyboard(cx);
    assert_eq!(
        prefs(cx).number_keys,
        desktop_core::app_state::NumberKeys::Workspaces
    );
    pick(handle, "settings-number-keys", 1, cx);
    assert_eq!(
        prefs(cx).number_keys,
        desktop_core::app_state::NumberKeys::Agents
    );
    saved(cx, &state, |value| {
        value["shell"]["numberKeys"] == json!("agents")
    });
}

#[gpui_kit::test]
fn tab_brings_the_focused_shortcut_into_view(cx: &mut TestAppContext) {
    let (_dir, _, handle) = open_keyboard(cx);
    for _ in 0..40 {
        press(handle, "tab", cx);
    }
    with_window(handle, cx, |window, _| {
        let list = window.find("settings-shortcuts").bounds();
        let focused = window
            .elements()
            .into_iter()
            .filter(|element| {
                element.path().last().is_some_and(|id| {
                    let id = id.to_string();
                    id.contains("shortcut-keys-") && !id.contains("shortcut-keys-cell-")
                })
            })
            .find(|keys| keys.focused() == Some(true))
            .expect("a shortcut's keys have the keyboard");
        let keys = focused.bounds();
        assert!(
            keys.top() >= list.top() && keys.bottom() <= list.bottom(),
            "{keys:?} in {list:?}"
        );
        // Far enough down that the list had to move: its first row is gone.
        let first = window
            .elements()
            .into_iter()
            .find(|keys| {
                let list = "settings-shortcuts".into();
                keys.path().contains(&list) && keys.path().last() != Some(&list)
            })
            .expect("a first row");
        assert!(!first.visible(), "the list scrolled");
    });
}
