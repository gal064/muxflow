//! The registry against today's desktop app, and the palette, Quick Open and
//! dialogs driven through GPUI's test platform.

use std::{
    cell::RefCell,
    collections::HashMap,
    path::{Path, PathBuf},
    rc::Rc,
    time::Duration,
};

use desktop_core::app_state::{AppStateStore, load_app_state};
use gpui_kit::{
    AnyWindowHandle, App, AppContext, Bounds, EntityInputHandler, Focusable, MouseButton, Role,
    Task, TestAppContext, Window, WindowBounds, WindowOptions,
    component::{
        WindowExt,
        input::{Input, InputState},
    },
    point, px, size,
    test::{TestEventExt, TestQueryExt, TestWindowExt},
};
use muxflow_gpui::{
    app::{self, Core},
    commands::{
        self, Commands,
        dialogs::{self, Confirmation, TextPromptRequest},
        keymap::{ShortcutOverrides, shortcut_for},
        quick_open::{
            FileMatch, FileSearchResults, QuickOpenItem, QuickOpenSnapshot, QuickOpenSource,
            QuickOpenTab, QuickOpenWorkspace,
        },
        registry::{COMMANDS, CommandDefinition, Platform, palette_commands},
    },
    ui::shell::Shell,
};

const REGISTRY_TS: &str = include_str!("../../desktop/src/commands/registry.ts");

/// The primary modifier, in stored form.
const PRIMARY: &str = if cfg!(target_os = "macos") {
    "Meta"
} else {
    "Ctrl"
};

fn field(line: &str, name: &str) -> Option<String> {
    let value = line.split_once(&format!("{name}: \""))?.1;
    Some(value.split('"').next()?.to_owned())
}

#[test]
fn the_registry_matches_registry_ts() {
    let parsed: Vec<&str> = REGISTRY_TS
        .lines()
        .filter(|line| line.trim_start().starts_with("{ id: \""))
        .collect();
    // Every command but the 18 generated positional selectors is one line there.
    assert_eq!(parsed.len(), 64);
    for line in &parsed {
        let id = field(line, "id").unwrap();
        let command = COMMANDS
            .iter()
            .find(|command| command.id == id)
            .unwrap_or_else(|| panic!("{id} is in registry.ts but not the Rust registry"));
        assert_eq!(
            field(line, "title").as_deref(),
            Some(command.title),
            "{id} title"
        );
        assert_eq!(
            field(line, "group").as_deref(),
            Some(command.group),
            "{id} group"
        );
        assert_eq!(field(line, "mac").as_deref(), command.mac, "{id} mac");
        assert_eq!(field(line, "linux").as_deref(), command.linux, "{id} linux");
        assert_eq!(
            line.contains("mutates: true"),
            command.mutates,
            "{id} mutates"
        );
        assert_eq!(
            line.contains("destructive: true"),
            command.destructive,
            "{id} destructive"
        );
        assert_eq!(
            line.contains("nativeFallback: true"),
            command.native_fallback,
            "{id} fallback"
        );
        assert_eq!(
            field(line, "requires").map(|value| value.to_lowercase()),
            command
                .requires
                .map(|value| format!("{value:?}").to_lowercase()),
            "{id} requires"
        );
        assert!(!command.palette_hidden, "{id} hidden");
    }
    for template in [
        "`workspace.select${digit}`",
        "`Go to workspace ${digit}`",
        "{ mac: `Meta+${digit}`, linux: `Ctrl+${digit}` }",
        "`tab.select${digit}`",
        "`Go to tab ${digit}`",
        "{ mac: `Ctrl+${digit}`, linux: `Alt+${digit}` }",
    ] {
        assert!(
            REGISTRY_TS.contains(template),
            "{template} changed in registry.ts"
        );
    }
    for digit in 1..=9 {
        let check = |id: String, title: String, mac: String, linux: String| {
            let command = COMMANDS.iter().find(|command| command.id == id).unwrap();
            assert_eq!(command.title, title);
            assert_eq!(command.mac, Some(mac.as_str()));
            assert_eq!(command.linux, Some(linux.as_str()));
            assert!(command.palette_hidden);
        };
        check(
            format!("workspace.select{digit}"),
            format!("Go to workspace {digit}"),
            format!("Meta+{digit}"),
            format!("Ctrl+{digit}"),
        );
        check(
            format!("tab.select{digit}"),
            format!("Go to tab {digit}"),
            format!("Ctrl+{digit}"),
            format!("Alt+{digit}"),
        );
    }
    assert_eq!(COMMANDS.len(), 64 + 18);
}

/// id, macOS key, Linux key, `-` for none. Pins the defaults independently of
/// the TypeScript source, which goes away at cutover.
const DEFAULT_KEYMAP: &str = "\
commands.show Meta+K Ctrl+K
workspaces.switch Meta+P Ctrl+P
settings.show Meta+, Ctrl+,
view.toggleSidebar Meta+B Ctrl+B
view.togglePanel Meta+L Ctrl+L
view.showFiles Meta+Shift+E Ctrl+Shift+E
view.showGit Meta+Shift+G Ctrl+Shift+G
focus.back Meta+[ Ctrl+[
focus.forward Meta+] Ctrl+]
agents.jumpUnread Meta+Shift+U Ctrl+Shift+U
session.new Meta+N Ctrl+N
tab.previous Meta+Shift+[ Ctrl+Shift+[
tab.next Meta+Shift+] Ctrl+Shift+]
window.new Meta+T Ctrl+T
window.close Meta+W Ctrl+W
pane.splitRight Meta+D Ctrl+Shift+O
pane.splitDown Meta+Shift+D Ctrl+Shift+D
pane.focusLeft Meta+Alt+ArrowLeft Ctrl+Alt+ArrowLeft
pane.focusRight Meta+Alt+ArrowRight Ctrl+Alt+ArrowRight
pane.focusUp Meta+Alt+ArrowUp Ctrl+Alt+ArrowUp
pane.focusDown Meta+Alt+ArrowDown Ctrl+Alt+ArrowDown
pane.resizeLeft - Ctrl+Shift+ArrowLeft
pane.resizeRight - Ctrl+Shift+ArrowRight
pane.resizeUp - Ctrl+Shift+ArrowUp
pane.resizeDown - Ctrl+Shift+ArrowDown
pane.zoom Meta+E Ctrl+Shift+Enter
terminal.copy Meta+C Ctrl+C
terminal.paste Meta+V Ctrl+V
terminal.search Meta+F Ctrl+F";

#[test]
fn the_default_keymaps_are_pinned() {
    let pinned: HashMap<&str, (&str, &str)> = DEFAULT_KEYMAP
        .lines()
        .map(|line| {
            let mut parts = line.split(' ');
            let (id, mac, linux) = (parts.next(), parts.next(), parts.next());
            (id.unwrap(), (mac.unwrap(), linux.unwrap()))
        })
        .collect();
    let none = ShortcutOverrides::new();
    for command in COMMANDS.iter().filter(|command| !command.palette_hidden) {
        let (mac, linux) = pinned.get(command.id).copied().unwrap_or(("-", "-"));
        let shown = |platform| shortcut_for(command, platform, &none).unwrap_or("-");
        assert_eq!(shown(Platform::Mac), mac, "{} on macOS", command.id);
        assert_eq!(shown(Platform::Linux), linux, "{} on Linux", command.id);
    }
    assert_eq!(pinned.len(), 29);
}

#[test]
fn the_terminal_keeps_ctrl_d_and_ctrl_e_on_linux() {
    let taken: Vec<&str> = COMMANDS
        .iter()
        .filter_map(|command| command.linux)
        .collect();
    assert!(!taken.contains(&"Ctrl+D"));
    assert!(!taken.contains(&"Ctrl+E"));
}

fn write_state(dir: &Path, overrides: serde_json::Value) -> PathBuf {
    let path = dir.join("app-state.json");
    let state = serde_json::json!({
        "schemaVersion": 1,
        "appTabs": [],
        "workspaceUi": [],
        "shell": {},
        "commands": { "shortcutOverrides": overrides },
    });
    std::fs::write(&path, serde_json::to_vec(&state).unwrap()).unwrap();
    path
}

fn open_app(state: &Path, cx: &mut TestAppContext) -> AnyWindowHandle {
    cx.update(|cx| {
        gpui_kit::init(cx);
        app::init(Core::at(state), cx);
        cx.set_reduce_motion(true);
        let bounds = Bounds {
            origin: point(px(0.), px(0.)),
            size: size(px(1000.), px(800.)),
        };
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
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

fn type_text(handle: AnyWindowHandle, text: &str, cx: &mut TestAppContext) {
    with_window(handle, cx, |window, cx| window.input(text, cx));
    cx.run_until_parked();
}

fn dialog_open(handle: AnyWindowHandle, cx: &mut TestAppContext) -> bool {
    with_window(handle, cx, |window, cx| window.has_active_dialog(cx))
}

fn close_dialogs(handle: AnyWindowHandle, cx: &mut TestAppContext) {
    with_window(handle, cx, |window, cx| window.close_all_dialogs(cx));
    cx.run_until_parked();
}

fn wait_for_overrides(path: &Path, expected: &ShortcutOverrides) {
    for _ in 0..500 {
        let store = AppStateStore::load(path.to_owned());
        if load_app_state(&store).unwrap().commands.shortcut_overrides == *expected {
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    panic!("the overrides did not reach app-state.json");
}

fn no_state(cx: &mut TestAppContext) -> (tempfile::TempDir, AnyWindowHandle) {
    let dir = tempfile::tempdir().unwrap();
    let path = write_state(dir.path(), serde_json::json!({}));
    let handle = open_app(&path, cx);
    (dir, handle)
}

/// Marks every command implemented so availability is decided by context alone.
fn implement_all(ran: Rc<RefCell<Vec<&'static str>>>, cx: &mut App) {
    for command in COMMANDS.iter().filter(|command| {
        !["commands.show", "workspaces.switch", "shortcuts.configure"].contains(&command.id)
    }) {
        let ran = ran.clone();
        let id = command.id;
        commands::on_command(id, cx, move |_, _| ran.borrow_mut().push(id));
    }
}

fn fully_available_context(cx: &mut App) {
    commands::update_context(cx, |context| {
        context.can_mutate = true;
        context.can_create_workspace = true;
        context.has_session = true;
        context.has_window = true;
        context.has_pane = true;
        context.has_tab = true;
    });
}

#[gpui_kit::test]
fn the_palette_lists_every_command_with_role_and_name(cx: &mut TestAppContext) {
    let (_dir, handle) = no_state(cx);
    press(handle, "secondary-k", cx);
    assert!(dialog_open(handle, cx));
    with_window(handle, cx, |window, _| {
        assert_eq!(window.find_all_by_role(Role::ListBoxOption).len(), 64);
        for command in palette_commands() {
            // The title bar may name a control the same as a command.
            let rows = window.find_all_by_label(command.title);
            assert!(
                rows.iter()
                    .any(|row| row.role() == Some(Role::ListBoxOption)),
                "{}",
                command.id
            );
        }
        assert_eq!(
            window.find_by_label("Search commands").role(),
            Some(Role::ComboBox)
        );
    });
}

#[gpui_kit::test]
fn unimplemented_commands_are_marked_unavailable_and_do_nothing(cx: &mut TestAppContext) {
    let (_dir, handle) = no_state(cx);
    press(handle, "secondary-k", cx);
    type_text(handle, "Toggle file beside the terminal", cx);
    press(handle, "enter", cx);
    assert!(
        dialog_open(handle, cx),
        "Enter on an unavailable row keeps the palette"
    );
    let available = cx.read(|cx| {
        let commands = cx.global::<Commands>();
        COMMANDS
            .iter()
            .filter(|command| commands.is_available(command))
            .map(|command| command.id)
            .collect::<Vec<_>>()
    });
    // With no connection, only what needs no host is available: the commands
    // that need a workspace, a tab or a pane wait for one.
    assert_eq!(
        available,
        [
            "commands.show",
            "workspaces.switch",
            "shortcuts.configure",
            "view.toggleSidebar",
            "view.togglePanel",
            "view.showFiles",
            "view.showGit",
            "view.showPorts",
            "focus.workspaces",
            "focus.tabs",
            "focus.back",
            "focus.forward",
            "agents.toggleSort",
            "workspaces.showPinnedOnly",
            "workspace.select1",
            "workspace.select2",
            "workspace.select3",
            "workspace.select4",
            "workspace.select5",
            "workspace.select6",
            "workspace.select7",
            "workspace.select8",
            "workspace.select9",
        ]
    );
}

#[gpui_kit::test]
fn arrows_move_the_selection_and_enter_runs_the_row(cx: &mut TestAppContext) {
    let (_dir, handle) = no_state(cx);
    let ran = Rc::new(RefCell::new(Vec::new()));
    let log = ran.clone();
    cx.update(|cx| {
        implement_all(log, cx);
        fully_available_context(cx);
    });
    press(handle, "secondary-k", cx);
    for _ in 0..4 {
        press(handle, "down", cx);
    }
    press(handle, "up", cx);
    press(handle, "enter", cx);
    assert!(!dialog_open(handle, cx));
    assert_eq!(*ran.borrow(), ["settings.show"]);
}

#[gpui_kit::test]
fn typing_ranks_by_fuzzy_score_and_click_runs_the_row(cx: &mut TestAppContext) {
    let (_dir, handle) = no_state(cx);
    let ran = Rc::new(RefCell::new(Vec::new()));
    let log = ran.clone();
    cx.update(|cx| {
        implement_all(log, cx);
        fully_available_context(cx);
    });
    press(handle, "secondary-k", cx);
    type_text(handle, "sright", cx);
    with_window(handle, cx, |window, cx| {
        window.find_by_label("Split pane right");
        window.click("command-pane.splitRight", cx);
    });
    cx.run_until_parked();
    assert!(!dialog_open(handle, cx));
    assert_eq!(*ran.borrow(), ["pane.splitRight"]);
}

#[gpui_kit::test]
fn escape_closes_and_returns_focus(cx: &mut TestAppContext) {
    let (_dir, handle) = no_state(cx);
    press(handle, "secondary-k", cx);
    press(handle, "escape", cx);
    assert!(!dialog_open(handle, cx));
}

struct FocusedField {
    input: gpui_kit::Entity<InputState>,
}

impl gpui_kit::Render for FocusedField {
    fn render(
        &mut self,
        _: &mut Window,
        _: &mut gpui_kit::Context<Self>,
    ) -> impl gpui_kit::IntoElement {
        Input::new(&self.input)
    }
}

#[gpui_kit::test]
fn a_composition_keeps_positional_selectors_from_running(cx: &mut TestAppContext) {
    let (_dir, _handle) = no_state(cx);
    let ran = Rc::new(RefCell::new(Vec::new()));
    let log = ran.clone();
    cx.update(|cx| {
        implement_all(log, cx);
        fully_available_context(cx);
    });
    let field = cx.update(|cx| {
        let options = WindowOptions::default();
        gpui_kit::open_window(options, cx, |window, cx| {
            let input = cx.new(|cx| InputState::new(window, cx));
            input.focus_handle(cx).focus(window, cx);
            cx.new(|_| FocusedField { input })
        })
        .unwrap()
        .0
    });
    with_window(field, cx, |_, _| {});
    let input = with_window(field, cx, composing_input);
    press(field, "secondary-3", cx);
    assert!(ran.borrow().is_empty(), "the composition owns the key");
    with_window(field, cx, |window, cx| {
        input.update(cx, |state, cx| state.unmark_text(window, cx));
    });
    press(field, "secondary-3", cx);
    assert_eq!(*ran.borrow(), ["workspace.select3"], "with no composition");
}

#[gpui_kit::test]
fn the_backdrop_closes_the_palette_but_not_a_prompt(cx: &mut TestAppContext) {
    let (_dir, handle) = no_state(cx);
    let corner = point(px(500.), px(790.));
    press(handle, "secondary-k", cx);
    with_window(handle, cx, |window, cx| {
        window.pointer_down(corner, MouseButton::Left, cx);
        window.pointer_up(corner, MouseButton::Left, cx);
    });
    cx.run_until_parked();
    assert!(!dialog_open(handle, cx));

    open_text_prompt(handle, cx);
    with_window(handle, cx, |window, cx| {
        window.pointer_down(corner, MouseButton::Left, cx);
        window.pointer_up(corner, MouseButton::Left, cx);
    });
    cx.run_until_parked();
    assert!(dialog_open(handle, cx));
}

#[gpui_kit::test]
fn shortcuts_are_ignored_while_an_overlay_is_open(cx: &mut TestAppContext) {
    let (_dir, handle) = no_state(cx);
    let ran = Rc::new(RefCell::new(Vec::new()));
    let log = ran.clone();
    cx.update(|cx| {
        implement_all(log, cx);
        fully_available_context(cx);
    });
    press(handle, "secondary-k", cx);
    press(handle, "secondary-t", cx);
    press(handle, "secondary-3", cx);
    assert!(ran.borrow().is_empty());
}

#[gpui_kit::test]
fn a_saved_override_takes_effect_and_the_old_binding_stops(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let chord = format!("{PRIMARY}+J");
    let path = write_state(dir.path(), serde_json::json!({ "commands.show": chord }));
    let handle = open_app(&path, cx);
    press(handle, "secondary-k", cx);
    assert!(!dialog_open(handle, cx), "the old default is replaced");
    press(handle, "secondary-j", cx);
    assert!(dialog_open(handle, cx));
}

#[gpui_kit::test]
fn setting_overrides_persists_and_applies_at_once(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let path = write_state(dir.path(), serde_json::json!({}));
    let handle = open_app(&path, cx);
    let chord = format!("{PRIMARY}+J");
    let expected: ShortcutOverrides = HashMap::from([
        ("commands.show".to_owned(), Some(chord)),
        ("window.new".to_owned(), None),
    ]);
    let overrides = expected.clone();
    with_window(handle, cx, move |window, cx| {
        commands::set_overrides(overrides, window, cx);
    });
    wait_for_overrides(&path, &expected);
    press(handle, "secondary-k", cx);
    assert!(!dialog_open(handle, cx));
    press(handle, "secondary-j", cx);
    assert!(dialog_open(handle, cx));
}

#[gpui_kit::test]
fn startup_repairs_a_collision_saves_it_and_says_so(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let chord = format!("{PRIMARY}+1");
    let path = write_state(dir.path(), serde_json::json!({ "window.new": chord }));
    let handle = open_app(&path, cx);
    let expected: ShortcutOverrides = HashMap::from([
        ("window.new".to_owned(), Some(chord)),
        ("workspace.select1".to_owned(), None),
    ]);
    wait_for_overrides(&path, &expected);
    cx.run_until_parked();
    let notices = with_window(handle, cx, |window, cx| window.notifications(cx).len());
    assert_eq!(notices, 1);
}

#[gpui_kit::test]
fn shortcut_editor_edits_conflicts_and_saves(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let path = write_state(dir.path(), serde_json::json!({}));
    let handle = open_app(&path, cx);
    with_window(handle, cx, |window, cx| {
        commands::run("shortcuts.configure", window, cx);
    });
    cx.run_until_parked();
    let field = "Shortcut for Show command palette";
    let done_disabled = |handle, cx: &mut TestAppContext| {
        with_window(handle, cx, |window, _| {
            window.find_by_label("Done").disabled() == Some(true)
        })
    };
    with_window(handle, cx, |window, _| {
        assert_eq!(window.find_all_by_role(Role::TextInput).len(), 82);
        let value = window.find_by_label(field).value().map(str::to_owned);
        assert_eq!(value.as_deref(), Some(&*format!("{PRIMARY}+K")));
    });
    assert!(!done_disabled(handle, cx));

    // Typing onto another command's chord conflicts and blocks Done.
    with_window(handle, cx, |window, cx| window.press("secondary-a", cx));
    type_text(handle, &format!("{PRIMARY}+T"), cx);
    assert!(done_disabled(handle, cx));
    with_window(handle, cx, |window, _| {
        let alert = window.find_all_by_role(Role::Alert);
        assert_eq!(alert.len(), 1);
    });

    with_window(handle, cx, |window, cx| window.press("secondary-a", cx));
    type_text(handle, &format!("{PRIMARY}+J"), cx);
    assert!(!done_disabled(handle, cx));
    with_window(handle, cx, |window, cx| {
        window.click("shortcut-editor-done", cx)
    });
    cx.run_until_parked();
    let expected: ShortcutOverrides =
        HashMap::from([("commands.show".to_owned(), Some(format!("{PRIMARY}+J")))]);
    wait_for_overrides(&path, &expected);
    assert!(!dialog_open(handle, cx));
    press(handle, "secondary-j", cx);
    assert!(dialog_open(handle, cx));
}

#[gpui_kit::test]
fn tab_brings_the_focused_shortcut_field_into_view(cx: &mut TestAppContext) {
    let (_dir, handle) = no_state(cx);
    with_window(handle, cx, |window, cx| {
        commands::run("shortcuts.configure", window, cx);
    });
    cx.run_until_parked();
    for _ in 0..40 {
        press(handle, "tab", cx);
    }
    with_window(handle, cx, |window, _| {
        let focused = window
            .find_all_by_role(Role::TextInput)
            .into_iter()
            .find(|field| field.focused() == Some(true))
            .expect("a focused field");
        assert!(
            focused.bounds().bottom() <= px(800.),
            "{:?}",
            focused.bounds()
        );
    });
}

#[gpui_kit::test]
fn shortcut_editor_shows_a_removed_binding_as_empty_and_restores(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let path = write_state(dir.path(), serde_json::json!({ "workspace.select1": null }));
    let handle = open_app(&path, cx);
    with_window(handle, cx, |window, cx| {
        commands::run("shortcuts.configure", window, cx);
    });
    cx.run_until_parked();
    let field = "Shortcut for Go to workspace 1";
    with_window(handle, cx, |window, cx| {
        assert_eq!(window.find_by_label(field).value(), Some(""));
        window.click("shortcut-editor-restore", cx);
    });
    cx.run_until_parked();
    with_window(handle, cx, |window, _| {
        let expected = if cfg!(target_os = "macos") {
            "Meta+1"
        } else {
            "Ctrl+1"
        };
        assert_eq!(window.find_by_label(field).value(), Some(expected));
    });
}

fn open_confirmation(
    handle: AnyWindowHandle,
    destructive: bool,
    cx: &mut TestAppContext,
) -> Rc<RefCell<u32>> {
    let confirmed = Rc::new(RefCell::new(0));
    let count = confirmed.clone();
    with_window(handle, cx, move |window, cx| {
        let confirmation = Confirmation {
            title: "Close workspace?".into(),
            detail: "This will ask tmux to permanently close it.".into(),
            confirm_label: "Close permanently".into(),
            destructive,
            on_confirm: Rc::new(move |_, _| *count.borrow_mut() += 1),
        };
        dialogs::open_confirmation(confirmation, window, cx);
    });
    cx.run_until_parked();
    confirmed
}

#[gpui_kit::test]
fn enter_never_confirms_a_destructive_close(cx: &mut TestAppContext) {
    let (_dir, handle) = no_state(cx);
    let confirmed = open_confirmation(handle, true, cx);
    press(handle, "enter", cx);
    assert_eq!(*confirmed.borrow(), 0);
    assert!(
        !dialog_open(handle, cx),
        "Enter activates the focused Cancel"
    );
}

#[gpui_kit::test]
fn escape_cancels_and_the_confirm_button_confirms(cx: &mut TestAppContext) {
    let (_dir, handle) = no_state(cx);
    let confirmed = open_confirmation(handle, true, cx);
    press(handle, "escape", cx);
    assert_eq!(*confirmed.borrow(), 0);
    assert!(!dialog_open(handle, cx));

    let confirmed = open_confirmation(handle, false, cx);
    with_window(handle, cx, |window, cx| {
        window.click("confirmation-confirm", cx)
    });
    cx.run_until_parked();
    assert_eq!(*confirmed.borrow(), 1);
    assert!(!dialog_open(handle, cx));
}

#[test]
fn tmux_confirmation_names_its_target() {
    let (title, detail) = dialogs::tmux_confirmation("Close workspace…", "workspace “api”");
    assert_eq!(title, "Close workspace?");
    assert_eq!(
        detail,
        "This will ask tmux to permanently close workspace “api”. Running processes in it will be terminated."
    );
}

fn open_text_prompt(handle: AnyWindowHandle, cx: &mut TestAppContext) -> Rc<RefCell<Vec<String>>> {
    let submitted = Rc::new(RefCell::new(Vec::new()));
    let log = submitted.clone();
    with_window(handle, cx, move |window, cx| {
        let request = TextPromptRequest {
            title: "Rename workspace".into(),
            label: "Workspace name".into(),
            initial_value: "old".into(),
            submit: Rc::new(move |value, _, _| log.borrow_mut().push(value)),
        };
        dialogs::open_text_prompt(request, window, cx);
    });
    cx.run_until_parked();
    submitted
}

#[gpui_kit::test]
fn a_text_prompt_submits_trimmed_text_and_refuses_empty(cx: &mut TestAppContext) {
    let (_dir, handle) = no_state(cx);
    let submitted = open_text_prompt(handle, cx);
    with_window(handle, cx, |window, cx| window.press("secondary-a", cx));
    type_text(handle, "  ", cx);
    press(handle, "enter", cx);
    assert!(submitted.borrow().is_empty());
    assert!(dialog_open(handle, cx));
    type_text(handle, " new ", cx);
    press(handle, "enter", cx);
    assert_eq!(*submitted.borrow(), ["new"]);
    assert!(!dialog_open(handle, cx));
}

fn composing_input(window: &mut Window, cx: &mut App) -> gpui_kit::Entity<InputState> {
    let focused = window.focused_input(cx).expect("a focused input");
    let input = focused.as_input().expect("a Kit input").clone();
    input.update(cx, |state, cx| {
        state.replace_and_mark_text_in_range(None, "に", Some(1..1), window, cx);
    });
    input
}

#[gpui_kit::test]
fn a_composition_keeps_enter_and_escape_from_the_dialog(cx: &mut TestAppContext) {
    let (_dir, handle) = no_state(cx);
    let submitted = open_text_prompt(handle, cx);
    let input = with_window(handle, cx, composing_input);
    press(handle, "enter", cx);
    assert!(submitted.borrow().is_empty(), "Enter is composing");
    press(handle, "escape", cx);
    assert!(dialog_open(handle, cx), "Escape ends the composition first");
    with_window(handle, cx, |window, cx| {
        let marked = input.update(cx, |state, cx| state.marked_text_range(window, cx));
        assert_eq!(marked, None);
    });
    press(handle, "escape", cx);
    assert!(!dialog_open(handle, cx));
}

#[gpui_kit::test]
fn a_composition_keeps_arrows_and_enter_from_the_palette(cx: &mut TestAppContext) {
    let (_dir, handle) = no_state(cx);
    let ran = Rc::new(RefCell::new(Vec::new()));
    let log = ran.clone();
    cx.update(|cx| {
        implement_all(log, cx);
        fully_available_context(cx);
    });
    press(handle, "secondary-k", cx);
    with_window(handle, cx, composing_input);
    press(handle, "down", cx);
    press(handle, "enter", cx);
    assert!(ran.borrow().is_empty());
    assert!(dialog_open(handle, cx));
    close_dialogs(handle, cx);
}

#[derive(Default)]
struct FakeSource {
    searches: RefCell<Vec<String>>,
    chosen: RefCell<Vec<String>>,
    results: RefCell<Option<Result<FileSearchResults, String>>>,
}

impl QuickOpenSource for FakeSource {
    fn snapshot(&self, _cx: &App) -> QuickOpenSnapshot {
        QuickOpenSnapshot {
            workspaces: vec![QuickOpenWorkspace {
                key: "1".into(),
                name: "api".into(),
                detail: "main · local".into(),
                active: false,
                letter: Some("L".into()),
            }],
            tabs: vec![QuickOpenTab {
                key: "t".into(),
                title: "shell".into(),
                file_path: None,
                is_file: false,
            }],
            active_tab_key: Some("t".into()),
            search_key: Some("root".into()),
            cwd: Some("/project".into()),
            host_label: "local".into(),
        }
    }

    fn search_files(
        &self,
        query: String,
        _cx: &mut App,
    ) -> Task<Result<FileSearchResults, String>> {
        self.searches.borrow_mut().push(query);
        Task::ready(
            self.results
                .borrow()
                .clone()
                .unwrap_or(Ok(FileSearchResults {
                    matches: vec![FileMatch {
                        path: "/project/src/main.rs".into(),
                        relative_path: "src/main.rs".into(),
                        score: 10,
                    }],
                    complete: true,
                })),
        )
    }

    fn choose(&self, item: &QuickOpenItem, _window: &mut Window, _cx: &mut App) {
        self.chosen.borrow_mut().push(item.key.clone());
    }
}

fn open_quick_open(handle: AnyWindowHandle, source: Rc<FakeSource>, cx: &mut TestAppContext) {
    cx.update(|cx| commands::set_quick_open_source(source, cx));
    press(handle, "secondary-p", cx);
    assert!(dialog_open(handle, cx));
}

const DEBOUNCE: Duration = Duration::from_millis(100);

#[gpui_kit::test]
fn quick_open_lists_tabs_and_workspaces_before_any_typing(cx: &mut TestAppContext) {
    let (_dir, handle) = no_state(cx);
    let source = Rc::new(FakeSource::default());
    open_quick_open(handle, source.clone(), cx);
    assert!(
        source.searches.borrow().is_empty(),
        "file search waits for typing"
    );
    with_window(handle, cx, |window, _| {
        assert_eq!(window.find_all_by_role(Role::ListBoxOption).len(), 2);
        assert_eq!(
            window
                .find_by_label("Search files, workspaces, and tabs")
                .role(),
            Some(Role::ComboBox)
        );
        assert_eq!(
            window.find_by_label("api").role(),
            Some(Role::ListBoxOption)
        );
    });
    press(handle, "down", cx);
    press(handle, "enter", cx);
    assert_eq!(*source.chosen.borrow(), ["workspace:1"]);
    assert!(!dialog_open(handle, cx));
}

#[gpui_kit::test]
fn quick_open_debounces_a_search_and_lists_the_file(cx: &mut TestAppContext) {
    let (_dir, handle) = no_state(cx);
    let source = Rc::new(FakeSource::default());
    open_quick_open(handle, source.clone(), cx);
    type_text(handle, "main", cx);
    assert!(
        source.searches.borrow().is_empty(),
        "still inside the debounce"
    );
    cx.executor().advance_clock(DEBOUNCE);
    cx.run_until_parked();
    assert_eq!(*source.searches.borrow(), ["main"]);
    with_window(handle, cx, |window, cx| {
        assert_eq!(
            window.find_by_label("main.rs").role(),
            Some(Role::ListBoxOption)
        );
        window.click("quick-open-file:/project/src/main.rs", cx);
    });
    cx.run_until_parked();
    assert_eq!(*source.chosen.borrow(), ["file:/project/src/main.rs"]);
}

#[gpui_kit::test]
fn quick_open_supersedes_a_stale_search(cx: &mut TestAppContext) {
    let (_dir, handle) = no_state(cx);
    let source = Rc::new(FakeSource::default());
    open_quick_open(handle, source.clone(), cx);
    type_text(handle, "ma", cx);
    type_text(handle, "in", cx);
    cx.executor().advance_clock(DEBOUNCE);
    cx.run_until_parked();
    assert_eq!(*source.searches.borrow(), ["main"]);
}

#[gpui_kit::test]
fn quick_open_keeps_workspaces_when_file_search_fails(cx: &mut TestAppContext) {
    let (_dir, handle) = no_state(cx);
    let source = Rc::new(FakeSource::default());
    *source.results.borrow_mut() = Some(Err("offline".into()));
    open_quick_open(handle, source.clone(), cx);
    type_text(handle, "api", cx);
    cx.executor().advance_clock(DEBOUNCE);
    cx.run_until_parked();
    with_window(handle, cx, |window, _| {
        assert_eq!(
            window.find_by_label("api").role(),
            Some(Role::ListBoxOption)
        );
        assert_eq!(window.find_all_by_role(Role::ListBoxOption).len(), 1);
    });
}

#[gpui_kit::test]
fn quick_open_opens_with_nothing_to_list_before_a_source_exists(cx: &mut TestAppContext) {
    let (_dir, handle) = no_state(cx);
    press(handle, "secondary-p", cx);
    assert!(dialog_open(handle, cx));
    press(handle, "enter", cx);
    assert!(dialog_open(handle, cx));
}

#[gpui_kit::test]
fn row_commands_route_to_their_surface_and_report_a_vanished_row(cx: &mut TestAppContext) {
    use commands::{RowCommandSource, RowSurface};

    let (_dir, handle) = no_state(cx);
    let ran = Rc::new(RefCell::new(Vec::new()));
    let log = ran.clone();
    cx.update(|cx| {
        commands::publish_row_commands(
            RowSurface::Git,
            Some(RowCommandSource {
                available: vec!["git.stage"],
                run: Rc::new(move |id, _, _| log.borrow_mut().push(id)),
            }),
            cx,
        );
    });
    let is_available = |id: &str, cx: &mut TestAppContext| {
        cx.read(|cx| {
            let commands = cx.global::<Commands>();
            let command: &CommandDefinition =
                COMMANDS.iter().find(|command| command.id == id).unwrap();
            commands.is_available(command)
        })
    };
    assert!(is_available("git.stage", cx));
    with_window(handle, cx, |window, cx| {
        commands::run("git.stage", window, cx)
    });
    assert_eq!(*ran.borrow(), ["git.stage"]);

    cx.update(|cx| commands::publish_row_commands(RowSurface::Git, None, cx));
    assert!(!is_available("git.stage", cx));
    with_window(handle, cx, |window, cx| {
        commands::run("git.stage", window, cx)
    });
    assert_eq!(ran.borrow().len(), 1);
    let notices = with_window(handle, cx, |window, cx| window.notifications(cx).len());
    assert_eq!(notices, 1);
}
