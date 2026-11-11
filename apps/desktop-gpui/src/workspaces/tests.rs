//! End-to-end: the shell in a real window against a fake host. The host's
//! frames go through the real bridge, engine and reducers; tmux actions and
//! input are recorded and answered here.

use std::{
    collections::VecDeque,
    path::Path,
    sync::{Arc, Mutex},
};

use desktop_core::{
    app_state::{AppStateStore, PersistedAppState},
    connection::{ConnectionSpec, ProfileStore, helper::HelperInstallReport},
    platform::{EventBody, EventSink},
};
use futures::future::{BoxFuture, FutureExt};
use gpui_kit::{
    AppContext, Entity, Role, TestAppContext, VisualTestContext, px, size,
    test::{TestQueryExt, TestWindowExt},
};
use serde_json::{Value, json};
use tmux_control::{Pane, TmuxSnapshot};

use super::{
    Workspaces, current,
    model::fixtures::{pane, session, window},
    moved_cursor,
};
use crate::{
    app::{self, Core},
    commands,
    connection_ui::{
        ConnectionUi,
        backend::{ConnectionBackend, EngineChannel, TerminalBackend, inline::InlineEngine},
        controller::ConnectionController,
        helper_upgrade::RemoteHelperProbe,
    },
    terminal::link::POLL_IDLE,
};

#[derive(Default)]
struct HostState {
    sinks: Vec<Arc<dyn EventSink>>,
    actions: Vec<Value>,
    answers: VecDeque<Result<Value, String>>,
    inputs: Vec<(String, Vec<u8>)>,
    generation: u64,
}

#[derive(Clone, Default)]
struct FakeHost(Arc<Mutex<HostState>>);

impl FakeHost {
    fn state(&self) -> std::sync::MutexGuard<'_, HostState> {
        self.0.lock().unwrap()
    }

    fn kinds(&self) -> Vec<String> {
        self.state()
            .actions
            .iter()
            .map(|action| action["kind"].as_str().unwrap_or_default().to_owned())
            .collect()
    }

    fn last(&self, kind: &str) -> Value {
        let found = self
            .state()
            .actions
            .iter()
            .rev()
            .find(|action| action["kind"] == kind)
            .cloned();
        found.unwrap_or_else(|| panic!("no {kind} in {:?}", self.kinds()))
    }
}

impl TerminalBackend for FakeHost {
    fn spawn_engine(
        &self,
        outputs: std::sync::mpsc::Sender<terminal_engine::EngineOutput>,
    ) -> Box<dyn EngineChannel> {
        Box::new(InlineEngine::new(outputs))
    }

    fn request_seed(&self, _: String, _: String) -> BoxFuture<'static, Result<(), String>> {
        async { Ok(()) }.boxed()
    }

    fn request_history(
        &self,
        _: String,
        _: String,
        _: u32,
        _: u32,
    ) -> BoxFuture<'static, Result<(), String>> {
        async { Ok(()) }.boxed()
    }

    fn set_pane_visibility(
        &self,
        _: String,
        _: String,
        _: bool,
        _: bool,
        _: u64,
        _: u64,
    ) -> BoxFuture<'static, Result<(), String>> {
        async { Ok(()) }.boxed()
    }

    fn resize_client(
        &self,
        _: String,
        _: u16,
        _: u16,
        _: String,
        _: String,
    ) -> BoxFuture<'static, Result<(), String>> {
        async { Ok(()) }.boxed()
    }

    fn send_input(&self, _: String, pane: String, bytes: Vec<u8>) -> Result<(), String> {
        self.state().inputs.push((pane, bytes));
        Ok(())
    }

    fn send_paste(&self, _: String, _: String, _: Vec<u8>) -> Result<(), String> {
        Ok(())
    }

    fn record_incident(&self, _: &str, _: Value) {}

    fn set_terminal_colors(&self, _: String, _: String) -> Result<(), String> {
        Ok(())
    }

    /// Answers what was queued, else what tmux would: the ids it acted on, a
    /// made-up id for what it created, and the next generation.
    fn tmux_action(&self, _: String, action: Value) -> BoxFuture<'static, Result<Value, String>> {
        let mut state = self.state();
        state.actions.push(action.clone());
        state.generation += 1;
        let generation = 100 + state.generation;
        let answer = state.answers.pop_front().unwrap_or_else(|| {
            let text = |key: &str| {
                action[key]
                    .as_str()
                    .filter(|value| !value.is_empty())
                    .map(str::to_owned)
            };
            let (mut session, mut window, mut pane) =
                (text("session_id"), text("window_id"), text("pane_id"));
            match action["kind"].as_str() {
                Some("createSession") => {
                    session = Some("$9".into());
                    window = Some("@9".into());
                    pane = Some("%9".into());
                }
                Some("createWindow") => {
                    window = Some("@8".into());
                    pane = Some("%8".into());
                }
                _ => {}
            }
            Ok(json!({
                "sessionId": session, "windowId": window, "paneId": pane,
                "topologyGeneration": generation,
            }))
        });
        async move { answer }.boxed()
    }

    fn start_upload(
        &self,
        _: crate::terminal::integration::transfers::UploadRequest,
        _: Arc<dyn EventSink>,
    ) -> Result<String, String> {
        Err("no uploads here".into())
    }

    fn cancel_upload(&self, _: String) -> Result<Value, String> {
        Ok(json!({}))
    }
}

impl ConnectionBackend for FakeHost {
    fn start_terminal(
        &self,
        _: String,
        _: ConnectionSpec,
        _: bool,
        sink: Arc<dyn EventSink>,
    ) -> Result<String, String> {
        let mut state = self.state();
        state.sinks.push(sink);
        Ok(format!("client-{}", state.sinks.len()))
    }

    fn stop_terminal(&self, _: String) {}

    fn acknowledge_delivery(
        &self,
        _: String,
        _: u64,
        _: u64,
        _: u64,
    ) -> BoxFuture<'static, Result<(), String>> {
        async { Ok(()) }.boxed()
    }

    fn probe_link(&self, _: String) -> BoxFuture<'static, Result<(), String>> {
        async { Ok(()) }.boxed()
    }

    fn discard_ssh_masters(&self) -> BoxFuture<'static, Result<(), String>> {
        async { Ok(()) }.boxed()
    }

    fn probe_helper(
        &self,
        _: ConnectionSpec,
    ) -> BoxFuture<'static, Result<RemoteHelperProbe, String>> {
        async { Err("not probed here".to_owned()) }.boxed()
    }

    fn install_helper(
        &self,
        _: ConnectionSpec,
        _: bool,
    ) -> BoxFuture<'static, Result<HelperInstallReport, String>> {
        async { Err("not installed here".to_owned()) }.boxed()
    }

    fn save_app_state(&self, _: PersistedAppState) -> BoxFuture<'static, Result<(), String>> {
        async { Ok(()) }.boxed()
    }

    fn late_requests(&self, _: &str) -> Option<u32> {
        None
    }

    fn now_ms(&self) -> u64 {
        1_000
    }

    fn network_online(&self) -> bool {
        true
    }
}

fn frame(kind: u8, label: &str, payload: &[u8]) -> Vec<u8> {
    let mut bytes = vec![kind];
    bytes.extend_from_slice(&(label.len() as u16).to_be_bytes());
    bytes.extend_from_slice(label.as_bytes());
    bytes.extend_from_slice(&0u64.to_be_bytes());
    bytes.extend_from_slice(payload);
    bytes
}

fn snapshot_frame(generation: u64, snapshot: &TmuxSnapshot) -> Vec<u8> {
    let body = json!({
        "sequence": 0, "generation": generation, "serverIdentity": "tmux",
        "authoritative": true, "snapshot": snapshot,
    });
    frame(7, "snapshot", body.to_string().as_bytes())
}

/// Two workspaces: `main` with two windows, the first split in two, and
/// `notes` with one.
fn world(active_window: &str) -> TmuxSnapshot {
    let windows = vec![
        window("@1", "$1", 0, "editor", active_window == "@1"),
        window("@2", "$1", 1, "server", active_window == "@2"),
        window("@3", "$2", 0, "shell", true),
    ];
    let panes: Vec<Pane> = vec![
        Pane {
            active: true,
            ..pane("%1", "@1", "$1", (0, 0, 40, 24))
        },
        pane("%2", "@1", "$1", (41, 0, 39, 24)),
        Pane {
            active: true,
            ..pane("%3", "@2", "$1", (0, 0, 80, 24))
        },
        Pane {
            active: true,
            ..pane("%4", "@3", "$2", (0, 0, 80, 24))
        },
    ];
    TmuxSnapshot {
        sessions: vec![session("$1", "main", 0), session("$2", "notes", 1)],
        windows,
        panes,
    }
}

struct Shell {
    host: FakeHost,
    visual: VisualTestContext,
    _dir: tempfile::TempDir,
}

impl Shell {
    fn workspaces(&mut self) -> Entity<Workspaces> {
        self.visual
            .update(|_, cx| current(cx).expect("the window made one"))
    }

    fn publish(&mut self, frames: Vec<Vec<u8>>) {
        let sink = self.host.state().sinks[0].clone();
        for frame in frames {
            sink.send(EventBody::Raw(frame)).unwrap();
        }
        self.settle();
    }

    fn settle(&mut self) {
        self.visual.run_until_parked();
        // What the engine produced waits for the link's next drain.
        self.visual.executor().advance_clock(POLL_IDLE);
        self.visual.run_until_parked();
    }

    fn run(&mut self, id: &'static str) {
        self.visual
            .update(|window, cx| commands::run(id, window, cx));
        self.settle();
    }

    fn shows(&mut self, label: &str) -> bool {
        self.visual
            .update(|window, _| window.try_find_by_label(label).is_some())
    }

    fn selected(&mut self, label: &str) -> Option<bool> {
        self.visual
            .update(|window, _| window.find_by_label(label).selected())
    }

    fn click(&mut self, id: &'static str) {
        self.visual.update(|window, cx| window.click(id, cx));
        self.settle();
    }
}

/// The app's window on a connected local host showing [`world`], with `tabs`
/// as the saved documents of `main`.
fn shell(cx: &mut TestAppContext, tabs: Value, shell: Value) -> Shell {
    let dir = tempfile::tempdir().unwrap();
    let profiles = dir.path().join("profiles.json");
    let saved = json!({
        "schemaVersion": 1, "lastProfileId": "local",
        "profiles": [{ "id": "local", "label": "Local", "connection": { "mode": "local" }, "shown": true }],
    });
    std::fs::write(&profiles, saved.to_string()).unwrap();
    let state_path = dir.path().join("app-state.json");
    let state = json!({
        "schemaVersion": 1, "appTabs": tabs, "shell": shell,
        "workspaceUi": [{
            "hostProfileId": "local", "serverIdentity": "tmux",
            "sessionId": "$1", "sessionName": "main",
        }],
    });
    std::fs::write(&state_path, state.to_string()).unwrap();
    let host = FakeHost::default();
    let backend: Arc<dyn ConnectionBackend> = Arc::new(host.clone());
    let store = Arc::new(ProfileStore::load(profiles).unwrap());
    let app_state = Arc::new(AppStateStore::load(state_path.clone()));
    let controller = cx.new(|cx| ConnectionController::new(backend, store, app_state, cx));
    cx.run_until_parked();
    let window = cx.update(|cx| {
        gpui_kit::init(cx);
        app::init(Core::at(Path::new(&state_path)), cx);
        cx.set_global(ConnectionUi(controller));
        app::open_main_window(cx).unwrap()
    });
    let visual = VisualTestContext::from_window(window, cx);
    visual.simulate_resize(size(px(1280.), px(800.)));
    let mut shell = Shell {
        host,
        visual,
        _dir: dir,
    };
    shell.publish(vec![
        frame(6, "connected", &[]),
        snapshot_frame(1, &world("@1")),
    ]);
    shell
}

fn doc(id: &str, title: &str, order: u32) -> Value {
    json!({
        "id": id, "hostProfileId": "local", "serverIdentity": "tmux",
        "sessionId": "$1", "sessionName": "main", "kind": "file",
        "resource": format!("/repo/{title}"), "title": title, "order": order,
    })
}

#[gpui_kit::test]
fn an_attached_host_lists_its_workspaces_and_the_host_row_names_it(cx: &mut TestAppContext) {
    let mut shell = shell(cx, json!([]), json!({}));
    assert!(!shell.shows("No tmux sessions on this host yet."));
    assert_eq!(shell.selected("main"), Some(true));
    assert_eq!(shell.selected("notes"), Some(false));
    assert!(shell.shows("Host Local over local, connected. Open the host menu."));
    assert!(!shell.shows("Local · Connected"), "the status pill is gone");
}

#[gpui_kit::test]
fn selecting_a_workspace_paints_at_once_and_asks_the_host(cx: &mut TestAppContext) {
    let mut shell = shell(cx, json!([]), json!({}));
    shell
        .visual
        .update(|window, cx| window.click(("workspace-row", 1usize), cx));
    shell.settle();
    assert_eq!(shell.selected("notes"), Some(true));
    let select = shell.host.last("selectSession");
    assert_eq!(select["session_id"], "$2");
    assert_eq!(select["expected_server_identity"], "tmux");
    // The guard holds against a snapshot that has not seen the switch.
    shell.publish(vec![snapshot_frame(2, &world("@1"))]);
    assert_eq!(shell.selected("notes"), Some(true));
}

#[gpui_kit::test]
fn a_refused_workspace_switch_is_put_back(cx: &mut TestAppContext) {
    let mut shell = shell(cx, json!([]), json!({}));
    shell
        .host
        .state()
        .answers
        .push_back(Err("tmux refused the switch".into()));
    shell
        .visual
        .update(|window, cx| window.click(("workspace-row", 1usize), cx));
    shell.settle();
    assert_eq!(shell.host.last("selectSession")["session_id"], "$2");
    // Nothing changed on the host, so no snapshot comes to correct the shell.
    assert_eq!(shell.selected("main"), Some(true));
    assert_eq!(shell.selected("notes"), Some(false));
}

#[gpui_kit::test]
fn choosing_a_workspace_hands_the_keyboard_to_its_terminal(cx: &mut TestAppContext) {
    let mut shell = shell(cx, json!([]), json!({}));
    let workspaces = shell.workspaces();
    shell.visual.update(|window, cx| {
        workspaces.update(cx, |this, cx| this.focus_list(window, cx));
    });
    shell.settle();
    shell
        .visual
        .update(|window, cx| window.click(("workspace-row", 0usize), cx));
    shell.settle();
    let (list, focused) = shell.visual.update(|window, cx| {
        (
            workspaces.read(cx).list_focus.is_focused(window),
            window.focused(cx).is_some(),
        )
    });
    assert!(!list, "the list let go");
    assert!(focused, "a pane took it");
}

#[gpui_kit::test]
fn the_list_cursor_starts_on_the_active_row_each_time_focus_arrives(cx: &mut TestAppContext) {
    let mut shell = shell(cx, json!([]), json!({}));
    let workspaces = shell.workspaces();
    shell.visual.update(|window, cx| {
        workspaces.update(cx, |this, cx| {
            this.focus_list(window, cx);
            this.list_cursor = Some(1);
        });
    });
    shell.settle();
    let focused = shell
        .visual
        .update(|window, cx| workspaces.read(cx).list_focus.is_focused(window));
    assert!(focused, "the list has the keyboard");
    // A switch made elsewhere must not leave the cursor behind.
    shell.visual.update(|_, cx| {
        workspaces.update(cx, |this, cx| this.select_window("@2", cx));
    });
    shell.settle();
    let (cursor, position) = shell.visual.update(|_, cx| {
        let this = workspaces.read(cx);
        (this.list_cursor, this.list_position(cx))
    });
    assert_eq!(cursor, None);
    assert_eq!(position, Some(0), "main, the active row");
}

#[gpui_kit::test]
fn focus_left_on_something_gone_comes_back_to_the_terminal(cx: &mut TestAppContext) {
    let mut shell = shell(cx, json!([]), json!({}));
    let workspaces = shell.workspaces();
    shell.visual.update(|window, cx| {
        workspaces.update(cx, |this, cx| this.focus_list(window, cx));
    });
    shell.settle();
    // As a menu that opened a dialog: its handle is focused and then gone.
    let stray = shell.visual.update(|window, cx| {
        let stray = cx.focus_handle();
        window.focus(&stray, cx);
        stray
    });
    shell.settle();
    let (stray, list, any) = shell.visual.update(|window, cx| {
        (
            stray.is_focused(window),
            workspaces.read(cx).list_focus.is_focused(window),
            window.focused(cx).is_some(),
        )
    });
    assert!(!stray && !list && any, "a pane has the keyboard");
}
#[gpui_kit::test]
fn split_and_close_reach_the_focused_pane(cx: &mut TestAppContext) {
    let mut shell = shell(cx, json!([]), json!({}));
    shell.run("pane.splitRight");
    let split = shell.host.last("splitPaneRight");
    assert_eq!(split["pane_id"], "%1");
    assert_eq!(split["split_size"], 50);
    shell.run("pane.splitDown");
    assert_eq!(shell.host.last("splitPaneDown")["pane_id"], "%1");
    // Close on a split tab closes the pane, confirmed and guarded on the server.
    shell.run("window.close");
    let close = shell.host.last("closePane");
    assert_eq!(close["pane_id"], "%1");
    assert_eq!(close["confirmed"], true);
    assert_eq!(close["expected_generation"], 0);
}

#[gpui_kit::test]
fn a_new_window_shows_a_placeholder_until_the_host_has_it(cx: &mut TestAppContext) {
    let mut shell = shell(cx, json!([]), json!({}));
    shell.run("window.new");
    assert_eq!(shell.host.last("createWindow")["session_id"], "$1");
    // The ack named @8, but no snapshot holds it yet.
    assert!(shell.shows("New window"));
    let mut next = world("@8");
    next.windows.push(window("@8", "$1", 2, "zsh", true));
    shell.publish(vec![snapshot_frame(3, &next)]);
    assert!(!shell.shows("New window"));
    assert_eq!(shell.selected("zsh"), Some(true));
}

#[gpui_kit::test]
fn creating_a_workspace_sends_its_startup_command_once(cx: &mut TestAppContext) {
    let mut shell = shell(cx, json!([]), json!({}));
    let workspaces = shell.workspaces();
    // Set in Settings, and in force at once.
    shell.visual.update(|window, cx| {
        crate::settings::update(
            |prefs| prefs.set_workspace_defaults("local", Some("/repo"), Some("make dev")),
            window,
            cx,
        );
        workspaces.update(cx, |this, cx| this.create_workspace("api".into(), cx));
    });
    shell.settle();
    let create = shell.host.last("createSession");
    assert_eq!(create["name"], "api");
    assert_eq!(create["directory"], "/repo");
    assert_eq!(
        shell.host.state().inputs,
        [("%9".to_owned(), b"make dev\n".to_vec())]
    );
}

#[gpui_kit::test]
fn closing_a_workspace_asks_first(cx: &mut TestAppContext) {
    let mut shell = shell(cx, json!([]), json!({}));
    shell.run("session.close");
    assert!(!shell.host.kinds().contains(&"closeSession".to_owned()));
    let open = shell.visual.update(|window, cx| {
        use gpui_kit::component::WindowExt;
        window.has_active_dialog(cx)
    });
    assert!(open, "the confirmation is up");
    assert!(shell.shows("Close permanently"));
    shell.click("confirmation-confirm");
    let close = shell.host.last("closeSession");
    assert_eq!(close["session_id"], "$1");
    assert_eq!(close["confirmed"], true);
}

#[gpui_kit::test]
fn back_returns_to_the_previous_window(cx: &mut TestAppContext) {
    let mut shell = shell(cx, json!([]), json!({}));
    shell.run("tab.next");
    assert_eq!(shell.host.last("selectWindow")["window_id"], "@2");
    shell.publish(vec![snapshot_frame(5, &world("@2"))]);
    assert_eq!(shell.selected("server"), Some(true));
    shell.run("focus.back");
    assert_eq!(shell.host.last("selectWindow")["window_id"], "@1");
    assert_eq!(shell.selected("editor"), Some(true));
}

#[gpui_kit::test]
fn the_command_context_follows_the_shell(cx: &mut TestAppContext) {
    let mut shell = shell(cx, json!([doc("a", "main.rs", 0)]), json!({}));
    let context = |shell: &mut Shell| {
        shell
            .visual
            .update(|_, cx| cx.global::<commands::Commands>().context().clone())
    };
    let now = context(&mut shell);
    assert!(now.can_mutate && now.has_session && now.has_window && now.has_pane && now.has_tab);
    assert!(now.can_create_workspace && now.can_move_session_down && !now.can_move_session_up);
    assert!(now.can_move_tab_right && !now.can_move_tab_left && !now.can_toggle_dock);
    shell.click("workspace-tab-app:a");
    let now = context(&mut shell);
    // A document covers the terminal: no window or pane to act on.
    assert!(!now.has_window && !now.has_pane && now.has_tab && now.can_toggle_dock);
}

// Ported from `tests/chrome.rs`, which had only saved placeholders to show.

#[gpui_kit::test]
fn tabs_are_named_tabs_and_selecting_one_changes_no_widths(cx: &mut TestAppContext) {
    let tabs = json!([doc("a", "main.rs", 0), doc("b", "lib.rs", 1)]);
    let mut shell = shell(cx, tabs, json!({ "tabsInTitleBar": false }));
    shell.visual.update(|window, _| {
        assert_eq!(window.find_by_label("main.rs").role(), Some(Role::Tab));
        assert_eq!(window.find_by_label("editor").selected(), Some(true));
    });
    let width = |shell: &mut Shell| {
        shell.visual.update(|window, _| {
            (
                window.find_by_label("main.rs").bounds().size.width,
                window.find_by_label("lib.rs").bounds().size.width,
                window.find_by_label("editor").bounds().size.width,
            )
        })
    };
    let before = width(&mut shell);
    shell.click("workspace-tab-app:b");
    assert_eq!(shell.selected("editor"), Some(false));
    assert_eq!(shell.selected("lib.rs"), Some(true));
    assert_eq!(before, width(&mut shell));
    for name in [
        "Close main.rs",
        "Close lib.rs",
        "Pin editor",
        "Close other tabs",
    ] {
        let role = shell
            .visual
            .update(|window, _| window.find_by_label(name).role());
        assert_eq!(role, Some(Role::Button), "{name}");
    }
}

#[gpui_kit::test]
fn the_all_tabs_button_follows_the_window_without_lagging(cx: &mut TestAppContext) {
    let tabs = json!([doc("a", "main.rs", 0), doc("b", "lib.rs", 1)]);
    let mut shell = shell(cx, tabs, json!({ "tabsInTitleBar": false }));
    assert!(!shell.shows("All tabs"));
    shell.visual.simulate_resize(size(px(200.), px(600.)));
    assert!(shell.shows("All tabs"), "the tabs no longer fit");
    shell.visual.simulate_resize(size(px(1280.), px(800.)));
    assert!(!shell.shows("All tabs"), "they fit again");
}

#[gpui_kit::test]
fn selecting_with_a_dock_open_follows_the_dock_rules(cx: &mut TestAppContext) {
    let tabs = json!([doc("a", "main.rs", 0), doc("b", "lib.rs", 1)]);
    let mut shell = shell(cx, tabs, json!({ "tabsInTitleBar": false }));
    let workspaces = shell.workspaces();
    shell
        .visual
        .update(|_, cx| workspaces.update(cx, |this, cx| this.dock_tab("b", None, cx)));
    shell.settle();
    // The docked document is already on screen.
    shell.click("workspace-tab-app:b");
    assert_eq!(shell.selected("editor"), Some(true));
    assert_eq!(shell.selected("lib.rs"), Some(false));
    // Another takes the dock, and the terminal keeps the main column.
    shell.click("workspace-tab-app:a");
    assert_eq!(shell.selected("main.rs"), Some(false));
    assert_eq!(shell.selected("editor"), Some(true));
    assert!(shell.shows("Resize the docked document"));
}

#[test]
fn a_moving_cursor_stays_in_its_list() {
    assert_eq!(moved_cursor(None, 3, "down", true), Some(1));
    assert_eq!(moved_cursor(Some(2), 3, "down", true), Some(2));
    assert_eq!(moved_cursor(Some(2), 3, "home", true), Some(0));
    assert_eq!(moved_cursor(Some(0), 3, "end", false), Some(2));
    assert_eq!(moved_cursor(Some(1), 3, "left", false), Some(0));
    assert_eq!(moved_cursor(Some(1), 3, "up", false), None);
    assert_eq!(moved_cursor(None, 0, "down", true), None);
}

#[test]
fn smooths_readings_and_forgets_a_stale_one() {
    use std::time::{Duration, Instant};

    use super::smoothed_latency;

    let start = Instant::now();
    let first = smoothed_latency(None, 100., start);
    assert_eq!(first.milliseconds, 100.);
    let second = smoothed_latency(Some(first), 200., start + Duration::from_secs(1));
    assert!((second.milliseconds - 130.).abs() < 1e-9);
    let late = smoothed_latency(Some(second), 50., start + Duration::from_secs(120));
    assert_eq!(late.milliseconds, 50.);
}
