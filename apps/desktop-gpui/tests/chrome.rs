//! The window chrome through GPUI's test platform: what shows, collapse and
//! restore, saved widths, and the accessibility baseline.

use std::{
    path::{Path, PathBuf},
    thread::sleep,
    time::{Duration, Instant},
};

use gpui_kit::{
    App, Role, TestAppContext, VisualTestContext, point, px, size,
    test::{TestQueryExt, TestWindowExt},
};
use muxflow_gpui::{
    app::{self, Core},
    commands,
    ui::shell::Shell,
};
use serde_json::{Value, json};

const SIDEBAR: &str = "Workspaces and agents";
const PANEL: &str = "Files, Git and Ports";

fn boot(state: &Path, cx: &mut App) {
    gpui_kit::init(cx);
    app::init(Core::at(state), cx);
}

fn write_state(dir: &Path, shell: Value) -> PathBuf {
    let path = dir.join("app-state.json");
    let tab = |id: &str, title: &str, order: u32| {
        json!({
            "id": id, "hostProfileId": "local", "serverIdentity": "tmux",
            "sessionId": "$1", "sessionName": "main", "kind": "file",
            "resource": format!("/repo/{title}"), "title": title, "order": order,
        })
    };
    let state = json!({
        "schemaVersion": 1,
        "appTabs": [tab("a", "main.rs", 0), tab("b", "lib.rs", 1)],
        "workspaceUi": [{
            "hostProfileId": "local", "serverIdentity": "tmux",
            "sessionId": "$1", "sessionName": "main", "selectedAppTabId": "a",
        }],
        "shell": shell,
    });
    std::fs::write(&path, serde_json::to_vec(&state).unwrap()).unwrap();
    path
}

fn open<'a>(state: &Path, cx: &'a mut TestAppContext) -> &'a mut VisualTestContext {
    cx.update(|cx| boot(state, cx));
    let (_, window_cx) = cx.add_window_view(Shell::new);
    window_cx.simulate_resize(size(px(1280.), px(800.)));
    window_cx
}

fn shows(window_cx: &mut VisualTestContext, label: &str) -> bool {
    window_cx.update(|window, _| window.try_find_by_label(label).is_some())
}

fn click(window_cx: &mut VisualTestContext, id: &'static str) {
    window_cx.update(|window, cx| window.click(id, cx));
}

/// Runs the app until `check` is true of the saved file. The save happens on
/// the backend's blocking pool, so the deterministic scheduler cannot reach it.
fn saved(cx: &mut VisualTestContext, path: &Path, check: impl Fn(&Value) -> bool) -> Value {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        cx.run_until_parked();
        let value: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        if check(&value) {
            return value;
        }
        assert!(Instant::now() < deadline, "not saved: {}", value["shell"]);
        sleep(Duration::from_millis(10));
    }
}

#[gpui_kit::test]
fn saved_rails_are_restored_with_their_widths(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let state = write_state(
        dir.path(),
        json!({ "panelOpen": true, "sidebarWidth": 300.0, "panelWidth": 320.0 }),
    );
    let window_cx = open(&state, cx);
    assert!(shows(window_cx, SIDEBAR));
    assert!(shows(window_cx, PANEL));
    let (sidebar, panel) = window_cx.update(|window, _| {
        (
            window.find("sidebar-resize").bounds().origin,
            window.find("panel-resize").bounds().origin,
        )
    });
    assert!(
        (sidebar.x.as_f32() + 8. - 300.).abs() < 3.,
        "sidebar: {sidebar:?}"
    );
    assert!(
        (1280. - panel.x.as_f32() - 320.).abs() < 3.,
        "panel: {panel:?}"
    );
}

#[gpui_kit::test]
fn a_toggle_collapses_saves_and_restores(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let state = write_state(dir.path(), json!({ "sidebarWidth": 300.0 }));
    let window_cx = open(&state, cx);

    assert!(shows(window_cx, SIDEBAR));
    assert!(!shows(window_cx, PANEL), "the panel is closed by default");

    click(window_cx, "title-sidebar-toggle");
    assert!(!shows(window_cx, SIDEBAR));
    let value = saved(window_cx, &state, |value| {
        value["shell"]["sidebarCollapsed"] == json!(true)
    });
    assert_eq!(value["shell"]["sidebarWidth"], json!(300.0));

    click(window_cx, "title-sidebar-toggle");
    assert!(shows(window_cx, SIDEBAR), "the sidebar comes back");
    assert_eq!(
        window_cx
            .update(|window, _| window.find("sidebar-resize").bounds().origin.x.as_f32())
            .round(),
        294.,
        "at its saved width"
    );

    click(window_cx, "title-panel-toggle");
    assert!(shows(window_cx, PANEL));
    saved(window_cx, &state, |value| {
        value["shell"]["panelOpen"] == json!(true)
    });
}

#[gpui_kit::test]
fn a_narrow_window_hides_rails_without_changing_the_preference(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let state = write_state(dir.path(), json!({ "panelOpen": true }));
    let window_cx = open(&state, cx);
    assert!(shows(window_cx, SIDEBAR) && shows(window_cx, PANEL));

    window_cx.simulate_resize(size(px(780.), px(600.)));
    assert!(!shows(window_cx, SIDEBAR), "the sidebar goes first");
    assert!(shows(window_cx, PANEL));

    window_cx.simulate_resize(size(px(600.), px(600.)));
    assert!(!shows(window_cx, PANEL));

    // Widening gives back exactly what was open: nothing was saved.
    window_cx.simulate_resize(size(px(1280.), px(800.)));
    assert!(shows(window_cx, SIDEBAR) && shows(window_cx, PANEL));
    let value: Value = serde_json::from_slice(&std::fs::read(&state).unwrap()).unwrap();
    assert_eq!(value["shell"]["panelOpen"], json!(true));
    assert_eq!(value["shell"]["sidebarCollapsed"], Value::Null);
}

#[gpui_kit::test]
fn dragging_the_sidebar_resizes_and_saves_once_released(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let state = write_state(dir.path(), json!({}));
    let window_cx = open(&state, cx);

    let from = window_cx.update(|window, _| {
        let bounds = window.find("sidebar-resize").bounds();
        point(
            bounds.origin.x + px(3.),
            bounds.origin.y + bounds.size.height / 2.,
        )
    });
    window_cx.update(|window, cx| window.drag(from, point(px(330.), from.y), cx));

    let value = saved(window_cx, &state, |value| {
        value["shell"]["sidebarWidth"]
            .as_f64()
            .is_some_and(|w| w > 300.)
    });
    let width = value["shell"]["sidebarWidth"].as_f64().unwrap();
    assert!((width - 327.).abs() < 6., "saved {width}");
    // A third of 1280 caps what a drag can ask for.
    let band = window_cx.update(|window, _| window.find("sidebar-resize").bounds().origin.x);
    let from = point(band + px(3.), from.y);
    window_cx.update(|window, cx| window.drag(from, point(px(900.), from.y), cx));
    let value = saved(window_cx, &state, |value| {
        value["shell"]["sidebarWidth"] == json!(426.0)
    });
    assert_eq!(value["shell"]["sidebarWidth"], json!(426.0));
}

#[gpui_kit::test]
fn selecting_a_surface_opens_the_panel_on_it(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let state = write_state(
        dir.path(),
        json!({ "panelOpen": true, "panelSurface": "git" }),
    );
    let window_cx = open(&state, cx);

    window_cx.update(|window, _| {
        let tab = window.find_by_label("Git");
        assert_eq!(tab.role(), Some(Role::Tab));
        assert_eq!(tab.selected(), Some(true));
    });
    click(window_cx, "panel-tab-ports");
    saved(window_cx, &state, |value| {
        value["shell"]["panelSurface"] == json!("ports")
    });
    window_cx.update(|window, _| {
        assert_eq!(window.find_by_label("Ports").selected(), Some(true));
    });
}

#[gpui_kit::test]
fn tabs_are_named_tabs_and_selecting_one_changes_no_widths(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let state = write_state(dir.path(), json!({ "tabsInTitleBar": false }));
    let window_cx = open(&state, cx);

    window_cx.update(|window, _| {
        assert_eq!(window.find_by_label("main.rs").role(), Some(Role::Tab));
        assert_eq!(window.find_by_label("main.rs").selected(), Some(true));
    });
    let width = |window_cx: &mut VisualTestContext| {
        window_cx.update(|window, _| {
            (
                window.find_by_label("main.rs").bounds().size.width,
                window.find_by_label("lib.rs").bounds().size.width,
            )
        })
    };
    let before = width(window_cx);
    click(window_cx, "workspace-tab-b");
    window_cx.update(|window, _| {
        assert_eq!(window.find_by_label("main.rs").selected(), Some(false));
        assert_eq!(window.find_by_label("lib.rs").selected(), Some(true));
    });
    assert_eq!(before, width(window_cx));
}

#[gpui_kit::test]
fn the_all_tabs_button_follows_the_window_without_lagging(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let state = write_state(dir.path(), json!({ "tabsInTitleBar": false }));
    let window_cx = open(&state, cx);
    assert!(!shows(window_cx, "All tabs"));

    window_cx.simulate_resize(size(px(200.), px(600.)));
    assert!(shows(window_cx, "All tabs"), "the tabs no longer fit");

    window_cx.simulate_resize(size(px(1280.), px(800.)));
    assert!(!shows(window_cx, "All tabs"), "they fit again");
}

#[gpui_kit::test]
fn selecting_with_a_dock_open_follows_the_dock_rules(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let state = write_state(dir.path(), json!({ "tabsInTitleBar": false }));
    let mut value: Value = serde_json::from_slice(&std::fs::read(&state).unwrap()).unwrap();
    value["workspaceUi"][0]["dockedAppTabId"] = json!("b");
    std::fs::write(&state, serde_json::to_vec(&value).unwrap()).unwrap();
    let window_cx = open(&state, cx);

    click(window_cx, "workspace-tab-b");
    window_cx.update(|window, _| {
        assert_eq!(window.find_by_label("main.rs").selected(), Some(true));
        assert_eq!(window.find_by_label("lib.rs").selected(), Some(false));
    });

    click(window_cx, "workspace-tab-a");
    window_cx.update(|window, _| {
        assert_eq!(window.find_by_label("main.rs").selected(), Some(false));
        assert_eq!(window.find_by_label("main.rs").role(), Some(Role::Tab));
        assert_eq!(window.find_by_label("lib.rs").selected(), Some(false));
    });
    assert!(shows(window_cx, "Resize the docked document"));
}

#[gpui_kit::test]
fn a_rail_stays_at_its_exact_breakpoint_when_scaled(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let state = write_state(dir.path(), json!({ "panelOpen": true }));
    let window_cx = open(&state, cx);
    window_cx.update(|window, _| window.set_rem_size(px(19.2)));

    window_cx.simulate_resize(size(px(1224.), px(800.)));
    assert!(shows(window_cx, SIDEBAR) && shows(window_cx, PANEL));

    window_cx.simulate_resize(size(px(1223.), px(800.)));
    assert!(!shows(window_cx, SIDEBAR), "one pixel under the breakpoint");
}

#[gpui_kit::test]
fn the_view_commands_drive_the_rails(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let state = write_state(dir.path(), json!({}));
    let window_cx = open(&state, cx);

    window_cx.update(|window, cx| commands::run("view.toggleSidebar", window, cx));
    assert!(!shows(window_cx, SIDEBAR));

    window_cx.update(|window, cx| commands::run("view.showGit", window, cx));
    assert!(shows(window_cx, PANEL));
    window_cx.update(|window, _| {
        assert_eq!(window.find_by_label("Git").selected(), Some(true));
    });

    window_cx.update(|window, cx| commands::run("view.togglePanel", window, cx));
    assert!(!shows(window_cx, PANEL));
}

#[gpui_kit::test]
fn every_control_is_a_named_control(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let state = write_state(dir.path(), json!({ "panelOpen": true }));
    let window_cx = open(&state, cx);

    window_cx.update(|window, _| {
        for name in [
            "Settings",
            "Back",
            "Forward",
            "Toggle sidebar",
            "Toggle right panel",
            "New terminal tab",
            "Close main.rs",
            "Close lib.rs",
            "Close other tabs",
            "Close all non-agent tabs",
            "New workspace",
        ] {
            assert_eq!(
                window.find_by_label(name).role(),
                Some(Role::Button),
                "{name}"
            );
        }
        for (name, role) in [
            ("Workspaces and agents", Role::Navigation),
            ("Files, Git and Ports", Role::Complementary),
            ("Workspace", Role::Main),
            ("Files", Role::Tab),
            ("Resize the sidebar", Role::Splitter),
            ("Resize the agents section", Role::Splitter),
            ("Resize the panel", Role::Splitter),
        ] {
            assert_eq!(window.find_by_label(name).role(), Some(role), "{name}");
        }
    });
}

#[gpui_kit::test]
fn a_broken_state_file_is_left_alone(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("app-state.json");
    std::fs::write(&path, b"{ not json").unwrap();
    let window_cx = open(&path, cx);
    assert!(shows(window_cx, SIDEBAR));

    click(window_cx, "title-sidebar-toggle");
    assert!(!shows(window_cx, SIDEBAR));
    // A recovered state refuses writes, so the file keeps what the user had.
    for _ in 0..10 {
        window_cx.run_until_parked();
        sleep(Duration::from_millis(5));
    }
    assert_eq!(std::fs::read(&path).unwrap(), b"{ not json");
}
