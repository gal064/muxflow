//! The window chrome through GPUI's test platform: what shows, collapse and
//! restore, saved widths, and the accessibility baseline.

use std::{
    path::{Path, PathBuf},
    thread::sleep,
    time::{Duration, Instant},
};

use desktop_core::app_state::ShellPreferences;
use gpui_kit::{
    App, Role, TestAppContext, VisualTestContext, point, px, size,
    test::{TestQueryExt, TestWindowExt},
};
use muxflow::{
    app::{self, Core},
    commands,
    ui::{chrome::layout, shell::Shell},
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

/// The left edge of each named control, which must all sit in the title bar.
fn title_bar_xs(window_cx: &mut VisualTestContext, labels: &[&str]) -> Vec<f32> {
    window_cx.update(|window, _| {
        let bar = window.find_by_label("Settings").bounds();
        labels
            .iter()
            .map(|label| {
                let bounds = window.find_by_label(label).bounds();
                assert!(
                    bounds.top() < bar.bottom() && bounds.bottom() > bar.top(),
                    "{label} is not in the title bar: {bounds:?}"
                );
                bounds.origin.x.as_f32()
            })
            .collect()
    })
}

fn in_order(window_cx: &mut VisualTestContext, labels: &[&str]) {
    let xs = title_bar_xs(window_cx, labels);
    for (pair, names) in xs.windows(2).zip(labels.windows(2)) {
        assert!(
            pair[0] < pair[1],
            "{} before {}: {xs:?}",
            names[0],
            names[1]
        );
    }
}

#[gpui_kit::test]
fn the_title_bar_keeps_todays_arrangement_without_tabs(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let state = write_state(dir.path(), json!({ "tabsInTitleBar": false }));
    let window_cx = open(&state, cx);
    in_order(
        window_cx,
        &[
            "Settings",
            "Toggle sidebar",
            "Back",
            "Forward",
            "No workspace",
            "No agents waiting",
            "New workspace",
            "Toggle right panel",
        ],
    );
    // No host is connected, so none can create a workspace.
    window_cx.update(|window, _| {
        assert_eq!(window.find_by_label("New workspace").disabled(), Some(true));
    });
}

#[gpui_kit::test]
fn with_tabs_the_title_bar_ends_with_the_rail_toggles(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let state = write_state(dir.path(), json!({}));
    let window_cx = open(&state, cx);
    // The strip's own toolbar stands for the tabs: the tab list is not observed.
    let tabs = "New terminal tab";
    in_order(
        window_cx,
        &[
            "Settings",
            "No agents waiting",
            "Back",
            "Forward",
            tabs,
            "Toggle sidebar",
            "Toggle right panel",
        ],
    );
    assert!(
        !shows(window_cx, "No workspace"),
        "the tabs replace the name"
    );
    // The open sidebar holds New workspace; the title bar does not repeat it.
    window_cx.update(|window, _| {
        let new = window.find_by_label("New workspace").bounds();
        let bar = window.find_by_label("Settings").bounds();
        assert!(new.top() >= bar.bottom(), "{new:?}");
    });

    click(window_cx, "title-sidebar-toggle");
    in_order(window_cx, &["Forward", "New workspace", tabs]);
    window_cx.update(|window, _| {
        assert_eq!(window.find_by_label("New workspace").disabled(), Some(true));
    });
}

#[gpui_kit::test]
fn the_panel_toggle_says_why_it_is_unavailable(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let state = write_state(dir.path(), json!({}));
    let window_cx = open(&state, cx);
    // 700 - 300 leaves the terminal less than its 480.
    window_cx.simulate_resize(size(px(700.), px(600.)));
    window_cx.update(|window, _| {
        let toggle = window.find_by_label("Toggle right panel");
        assert_eq!(toggle.disabled(), Some(true));
        assert_eq!(
            toggle.description(),
            Some("Widen the window to show Files and Git")
        );
    });
}

fn default_prefs(sidebar_width: f32, panel_width: f32, panel_open: bool) -> layout::Prefs {
    let mut prefs = layout::Prefs::from_shell(&ShellPreferences::default());
    prefs.sidebar_width = sidebar_width;
    prefs.panel_width = panel_width;
    prefs.panel_open = panel_open;
    prefs
}

#[test]
fn the_sidebar_gets_the_panels_room_when_the_panel_is_closed() {
    let sidebar_only = default_prefs(240., 300., false);
    let at = |width| {
        let rails = layout::rails(&sidebar_only, width);
        (rails.sidebar_open, rails.panel_open)
    };
    assert_eq!(at(240. + 480.), (true, false));
    assert_eq!(at(240. + 480. - 1.), (false, false));
}

#[test]
fn the_rails_measure_against_their_own_widths() {
    let at = |sidebar, panel| {
        let rails = layout::rails(&default_prefs(sidebar, panel, true), 1_200.);
        (rails.sidebar_open, rails.panel_open)
    };
    assert_eq!(at(240., 300.), (true, true));
    // A sidebar dragged wide gives way sooner than a default one.
    assert_eq!(at(400., 400.), (false, true));
}

#[gpui_kit::test]
fn asking_for_a_surface_twice_keeps_it_open(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let state = write_state(dir.path(), json!({}));
    let window_cx = open(&state, cx);
    for _ in 0..2 {
        window_cx.update(|window, cx| commands::run("view.showGit", window, cx));
        assert!(shows(window_cx, PANEL));
        window_cx.update(|window, _| {
            assert_eq!(window.find_by_label("Git").selected(), Some(true));
        });
    }
    window_cx.update(|window, cx| commands::run("view.showPorts", window, cx));
    assert!(shows(window_cx, PANEL));
    window_cx.update(|window, _| {
        assert_eq!(window.find_by_label("Ports").selected(), Some(true));
    });
    saved(window_cx, &state, |value| {
        value["shell"]["panelOpen"] == json!(true)
            && value["shell"]["panelSurface"] == json!("ports")
    });
}

#[gpui_kit::test]
fn a_rail_command_means_the_same_at_any_width(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let state = write_state(
        dir.path(),
        json!({ "sidebarCollapsed": true, "panelOpen": true }),
    );
    let window_cx = open(&state, cx);
    window_cx.simulate_resize(size(px(900.), px(800.)));
    window_cx.update(|window, cx| commands::run("view.toggleSidebar", window, cx));
    // Too narrow for the sidebar beside the panel: it waits, and the panel stays.
    assert!(!shows(window_cx, SIDEBAR));
    assert!(shows(window_cx, PANEL));
    saved(window_cx, &state, |value| {
        value["shell"]["sidebarCollapsed"] == json!(false)
            && value["shell"]["panelOpen"] == json!(true)
    });
    window_cx.simulate_resize(size(px(1280.), px(800.)));
    assert!(shows(window_cx, SIDEBAR) && shows(window_cx, PANEL));
}

#[gpui_kit::test]
fn dragging_the_panel_edge_resizes_it_within_its_bounds(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let state = write_state(dir.path(), json!({ "panelOpen": true }));
    let window_cx = open(&state, cx);
    let grab = |window_cx: &mut VisualTestContext| {
        window_cx.update(|window, _| {
            let bounds = window.find("panel-resize").bounds();
            point(
                bounds.origin.x + px(3.),
                bounds.origin.y + bounds.size.height / 2.,
            )
        })
    };
    let width = |value: &Value| value["shell"]["panelWidth"].as_f64().unwrap_or_default();

    let from = grab(window_cx);
    window_cx.update(|window, cx| window.drag(from, point(px(1280. - 400.), from.y), cx));
    let value = saved(window_cx, &state, |value| width(value) > 350.);
    assert!((width(&value) - 400.).abs() < 6., "saved {}", width(&value));

    // 1280 less the terminal's 480 and the sidebar's 240 caps a drag at 560.
    let from = grab(window_cx);
    window_cx.update(|window, cx| window.drag(from, point(px(100.), from.y), cx));
    let value = saved(window_cx, &state, |value| width(value) == 560.);
    assert_eq!(width(&value), 560.);

    // And it never gets narrower than 240.
    let from = grab(window_cx);
    window_cx.update(|window, cx| window.drag(from, point(px(1270.), from.y), cx));
    let value = saved(window_cx, &state, |value| width(value) == 240.);
    assert_eq!(width(&value), 240.);
}
