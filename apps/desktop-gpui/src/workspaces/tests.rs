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
    /// How many inputs to refuse first, as a bridge still reconnecting does.
    refused_inputs: usize,
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
        let mut state = self.state();
        if state.refused_inputs > 0 {
            state.refused_inputs -= 1;
            return Err("terminal bridge is disconnected or reconciling".into());
        }
        state.inputs.push((pane, bytes));
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
    fn tmux_action(
        &self,
        client: String,
        action: Value,
    ) -> BoxFuture<'static, Result<Value, String>> {
        let mut state = self.state();
        // Kept with the client it went to, for the tests that use two hosts.
        let mut recorded = action.clone();
        recorded["client"] = json!(client);
        state.actions.push(recorded);
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
        self.publish_to(0, frames);
    }

    fn publish_to(&mut self, bridge: usize, frames: Vec<Vec<u8>>) {
        let sink = self.host.state().sinks[bridge].clone();
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
    shell_with(cx, tabs, shell, &[])
}

/// [`shell`], with more shown hosts after the local one. Each gets its own
/// bridge, in order, and says nothing until [`Shell::publish_to`] it.
fn shell_with(cx: &mut TestAppContext, tabs: Value, shell: Value, more: &[Value]) -> Shell {
    let dir = tempfile::tempdir().unwrap();
    let profiles = dir.path().join("profiles.json");
    let mut listed = vec![
        json!({ "id": "local", "label": "Local", "connection": { "mode": "local" }, "shown": true }),
    ];
    listed.extend_from_slice(more);
    let saved = json!({
        "schemaVersion": 1, "lastProfileId": "local", "profiles": listed,
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

/// Port of the font size case in `TerminalPane.test.tsx`: the setting reaches
/// the panes already open, which keep their screens rather than remounting.
#[gpui_kit::test]
fn a_font_size_change_reaches_open_panes_without_remounting_them(cx: &mut TestAppContext) {
    let mut shell = shell(cx, json!([]), json!({}));
    shell.publish(vec![frame(10, "terminal", &1u64.to_be_bytes())]);
    let focused = |shell: &mut Shell| {
        shell
            .visual
            .update(|window, cx| crate::terminal::reading::focused_pane(window, cx))
            .expect("the terminal has a pane in focus")
    };
    let pane = focused(&mut shell);
    shell.visual.update(|window, cx| {
        crate::settings::update(|prefs| prefs.terminal_font_size = 19, window, cx)
    });
    shell.settle();
    assert_eq!(focused(&mut shell), pane, "the pane was remounted");
    let size = shell
        .visual
        .update(|_, cx| pane.read(cx).metrics().font_size);
    assert_eq!(size, px(19.));
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
            workspaces.read(cx).list_focus.contains_focused(window, cx),
            window.focused(cx).is_some(),
        )
    });
    assert!(!list, "the list let go");
    assert!(focused, "a pane took it");
}

/// The label of the row, its id starting `row`, that holds the keyboard.
fn focused_row(shell: &mut Shell, row: &'static str) -> Option<String> {
    use gpui_kit::ElementId::{Name, NamedInteger};
    shell.visual.update(|window, _| {
        window
            .elements()
            .into_iter()
            .filter(|element| match element.path().last() {
                Some(Name(id) | NamedInteger(id, _)) => id.starts_with(row),
                _ => false,
            })
            .find(|element| element.focused() == Some(true))
            .and_then(|element| element.label().map(str::to_owned))
    })
}

#[gpui_kit::test]
fn each_workspace_row_is_a_tab_stop_and_the_arrows_move_between_them(cx: &mut TestAppContext) {
    let mut shell = shell(cx, json!([]), json!({}));
    let workspaces = shell.workspaces();
    let label = |shell: &mut Shell, index: usize| {
        shell
            .visual
            .update(|_, cx| workspaces.read(cx).rows(true, cx)[index].label())
    };
    let (first, second) = (label(&mut shell, 0), label(&mut shell, 1));
    // `focus.workspaces` lands on the active row, main.
    shell
        .visual
        .update(|window, cx| workspaces.update(cx, |this, cx| this.focus_list(window, cx)));
    shell.settle();
    let list = "workspace-row";
    assert_eq!(focused_row(&mut shell, list), Some(first.clone()));
    shell.visual.simulate_keystrokes("down");
    shell.settle();
    assert_eq!(focused_row(&mut shell, list), Some(second.clone()));
    shell.visual.simulate_keystrokes("down");
    shell.settle();
    assert_eq!(
        focused_row(&mut shell, list),
        Some(second),
        "Down stops at the end"
    );
    shell.visual.simulate_keystrokes("up up");
    shell.settle();
    assert_eq!(focused_row(&mut shell, list), Some(first.clone()));
    // The pin is the pointer's: Tab goes on to the next row.
    shell.visual.simulate_keystrokes("tab");
    shell.settle();
    assert_eq!(focused_row(&mut shell, list), Some(label(&mut shell, 1)));
    shell.visual.simulate_keystrokes("shift-tab");
    shell.visual.update(|window, cx| window.press("enter", cx));
    shell.settle();
    let (list_held, active) = shell.visual.update(|window, cx| {
        let this = workspaces.read(cx);
        (
            this.list_focus.contains_focused(window, cx),
            this.rows(true, cx)[0].active,
        )
    });
    assert!(
        active && !list_held,
        "Enter chooses it and hands the keyboard on"
    );
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
            workspaces.read(cx).list_focus.contains_focused(window, cx),
            window.focused(cx).is_some(),
        )
    });
    assert!(!stray && !list && any, "a pane has the keyboard");
}

/// Port of `useBulkTabClose.test.tsx`'s recheck: presence is read when each
/// terminal's turn comes, not from the set, so one that may hold an agent
/// stays even though the set names it.
#[gpui_kit::test]
fn closing_non_agent_tabs_keeps_a_window_that_may_hold_an_agent(cx: &mut TestAppContext) {
    use super::agents::{AgentPresence, AgentShell, WindowAgents};

    let mut shell = shell(cx, json!([]), json!({}));
    let workspaces = shell.workspaces();
    // The set holds both windows, but only `@1` is known to be agent-free.
    let (scope, set) = shell.visual.update(|_, cx| {
        let view = workspaces.read(cx).view(cx);
        (view.scope.clone(), view.tabs.clone())
    });
    shell.visual.update(|_, cx| {
        let window = |presence| WindowAgents {
            attention: None,
            presence,
        };
        cx.set_global(AgentShell {
            windows: std::collections::HashMap::from([
                (("local".into(), "@1".into()), window(AgentPresence::Absent)),
                (
                    ("local".into(), "@2".into()),
                    window(AgentPresence::Present),
                ),
            ]),
            generations: std::collections::HashMap::from([("local".into(), u64::MAX)]),
            ..AgentShell::default()
        });
    });
    shell
        .visual
        .update(|_, cx| workspaces.update(cx, |this, cx| this.close_tab_set(scope, set, true, cx)));
    shell.settle();
    let closed: Vec<Value> = shell
        .host
        .state()
        .actions
        .iter()
        .filter(|action| action["kind"] == "closeWindow")
        .map(|action| action["window_id"].clone())
        .collect();
    assert_eq!(closed, [json!("@1")]);
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
fn a_startup_command_waits_for_a_host_still_reconnecting(cx: &mut TestAppContext) {
    let mut shell = shell(cx, json!([]), json!({}));
    let workspaces = shell.workspaces();
    // Creating the first workspace started the tmux server: the bridge
    // refuses input until it has reconnected.
    shell.host.state().refused_inputs = 3;
    shell.visual.update(|window, cx| {
        crate::settings::update(
            |prefs| prefs.set_workspace_defaults("local", None, Some("make dev")),
            window,
            cx,
        );
        workspaces.update(cx, |this, cx| this.create_workspace("api".into(), cx));
    });
    shell.settle();
    assert!(shell.host.state().inputs.is_empty());
    shell
        .visual
        .executor()
        .advance_clock(std::time::Duration::from_secs(1));
    shell.settle();
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
fn a_narrow_window_caps_tabs_tighter(cx: &mut TestAppContext) {
    let long = "a_file_name_long_enough_to_reach_the_cap_of_any_tab.rs";
    let tabs = json!([doc("a", long, 0)]);
    let mut shell = shell(cx, tabs, json!({ "tabsInTitleBar": false }));
    let width = |shell: &mut Shell| {
        shell.settle();
        shell
            .visual
            .update(|window, _| window.find_by_label(long).bounds().size.width)
    };
    let wide = width(&mut shell);
    assert!(wide > px(150.) && wide <= px(220.), "{wide:?}");
    shell.visual.simulate_resize(size(px(860.), px(700.)));
    assert!(width(&mut shell) <= px(150.));
}

/// Tabs follow each other with no gap, each as wide as Tauri's terminal `.tab`:
/// `.tab-select`'s 14px left padding, the number, a 6px gap, the title, 2px of right
/// padding, then the pin toggle's 18px slot and its 4px margin.
#[gpui_kit::test]
fn terminal_tabs_are_as_wide_as_in_tauri(cx: &mut TestAppContext) {
    let mut shell = shell(cx, json!([]), json!({ "tabsInTitleBar": false }));
    let (pitch, number, title) = shell.visual.update(|window, _| {
        let tab = |id: &'static str| window.find(id).bounds().left();
        let width = |family: &'static str, size: f32, text: &'static str| {
            let run = gpui_kit::TextRun {
                len: text.len(),
                font: gpui_kit::font(family),
                color: gpui_kit::Hsla::default(),
                background_color: None,
                underline: None,
                strikethrough: None,
            };
            window
                .text_system()
                .shape_line(text.into(), px(size), &[run], None)
                .width
        };
        (
            tab("workspace-tab-terminal:@2") - tab("workspace-tab-terminal:@1"),
            width(crate::fonts::MONO, crate::theme::metrics::TYPE_META, "1"),
            width(
                crate::fonts::SANS,
                crate::theme::metrics::TYPE_MENU,
                "editor",
            ),
        )
    });
    let expected = px(14.) + number + px(6.) + title + px(2.) + px(18.) + px(4.);
    // Layout rounds the number and the title each up to whole pixels.
    assert!(
        (pitch - expected).abs() < px(2.),
        "pitch {pitch:?}, expected {expected:?}"
    );
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

#[gpui_kit::test]
fn pane_focus_steps_into_the_docked_document_past_the_edge_and_back(cx: &mut TestAppContext) {
    let mut tab = doc("a", "main.rs", 0);
    // A root, so the document mounts a pane that can hold the keyboard.
    tab["rootPath"] = json!("/repo");
    tab["rootToken"] = json!("root");
    let mut shell = shell(cx, json!([tab]), json!({ "tabsInTitleBar": false }));
    let workspaces = shell.workspaces();
    shell.visual.update(|_, cx| {
        workspaces.update(cx, |this, cx| {
            this.dock_tab("a", Some(desktop_core::app_state::DockSide::Left), cx)
        })
    });
    shell.settle();
    let in_dock = |shell: &mut Shell| {
        shell.visual.update(|window, cx| {
            window.render_frame(cx);
            crate::editor::holds_focus(window)
        })
    };
    assert!(!in_dock(&mut shell));
    // %1 is the leftmost pane: off its edge toward the dock goes in.
    shell.run("pane.focusLeft");
    assert!(in_dock(&mut shell));
    // Toward the dock again stays; any other way comes back out.
    shell.run("pane.focusLeft");
    assert!(in_dock(&mut shell));
    shell.run("pane.focusUp");
    assert!(!in_dock(&mut shell));
    shell.run("pane.focusLeft");
    assert!(in_dock(&mut shell));
    shell.run("pane.focusRight");
    assert!(!in_dock(&mut shell));
}

#[gpui_kit::test]
fn focus_follows_the_mouse_onto_the_docked_document_and_back(cx: &mut TestAppContext) {
    let mut tab = doc("a", "main.rs", 0);
    tab["rootPath"] = json!("/repo");
    tab["rootToken"] = json!("root");
    let mut shell = shell(
        cx,
        json!([tab]),
        json!({ "tabsInTitleBar": false, "focusFollowsMouse": true }),
    );
    let workspaces = shell.workspaces();
    shell.visual.update(|window, cx| {
        window.activate_window();
        workspaces.update(cx, |this, cx| {
            this.dock_tab("a", Some(desktop_core::app_state::DockSide::Left), cx)
        })
    });
    shell.settle();
    let in_dock = |shell: &mut Shell| {
        shell.visual.update(|window, cx| {
            window.render_frame(cx);
            crate::editor::holds_focus(window)
        })
    };
    // The dock is on the left, so its divider is its right edge.
    let edge = shell.visual.update(|window, _| {
        window
            .find_by_label("Resize the docked document")
            .bounds()
            .center()
    });
    let rest_on = |shell: &mut Shell, at: gpui_kit::Point<gpui_kit::Pixels>| {
        shell
            .visual
            .simulate_mouse_move(at, None, gpui_kit::Modifiers::none());
        shell.visual.run_until_parked();
        shell
            .visual
            .executor()
            .advance_clock(crate::terminal::integration::frames::HOVER_FOCUS_DELAY);
        shell.visual.run_until_parked();
    };
    // The keyboard starts in the terminal pane.
    shell
        .visual
        .update(|window, cx| crate::workspaces::focus_terminal(&workspaces, window, cx));
    shell.settle();
    assert!(!in_dock(&mut shell));
    rest_on(&mut shell, gpui_kit::point(edge.x - px(60.), edge.y));
    assert!(
        in_dock(&mut shell),
        "resting on the dock takes the keyboard"
    );
    rest_on(&mut shell, gpui_kit::point(edge.x + px(60.), edge.y));
    assert!(!in_dock(&mut shell), "resting on a pane takes it back");

    // The dock goes away under the pointer, which so never leaves it; the
    // next docked document still takes the keyboard on a rest.
    let on_dock = gpui_kit::point(edge.x - px(60.), edge.y);
    rest_on(&mut shell, on_dock);
    shell
        .visual
        .update(|_, cx| workspaces.update(cx, |this, cx| this.undock(cx)));
    shell.settle();
    shell.visual.update(|window, cx| {
        workspaces.update(cx, |this, cx| {
            this.dock_tab("a", Some(desktop_core::app_state::DockSide::Left), cx)
        });
        crate::workspaces::focus_terminal(&workspaces, window, cx);
    });
    shell.settle();
    assert!(!in_dock(&mut shell));
    rest_on(&mut shell, on_dock);
    assert!(in_dock(&mut shell), "a new dock is a new entry");
}

#[test]
fn a_moving_cursor_stays_in_its_list() {
    assert_eq!(moved_cursor(None, 3, "right"), Some(1));
    assert_eq!(moved_cursor(Some(2), 3, "right"), Some(2));
    assert_eq!(moved_cursor(Some(2), 3, "home"), Some(0));
    assert_eq!(moved_cursor(Some(0), 3, "end"), Some(2));
    assert_eq!(moved_cursor(Some(1), 3, "left"), Some(0));
    assert_eq!(moved_cursor(Some(1), 3, "up"), None);
    assert_eq!(moved_cursor(None, 0, "right"), None);
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

/// The seams between the shell and the agents feature: the agents list reads
/// the sidebar, and every way to an agent's pane is the shell's own move.
mod agent_seams {
    use std::{cell::RefCell, collections::HashMap, rc::Rc, time::Duration};

    use desktop_core::{app_state::AgentSortMode, connection::agent::AgentCommand};

    use super::*;
    use crate::{
        agents::{
            self, Agents,
            activation::{self, Route},
            runtime::{AgentRuntime, AgentTransport},
            tests::{Effects, Recorded, snapshot_answer, wire_adapter, wire_agent},
        },
        connection_ui::ConnectionUi,
        terminal::integration::agents::PaneActivity,
        workspaces::model::terminal_key,
    };

    pub(super) type Requests = Rc<RefCell<Vec<(String, AgentCommand)>>>;

    /// What each `branch` request is answered with, in order.
    pub(super) type Branches = Vec<BoxFuture<'static, Result<Value, String>>>;

    /// Each host's agents, told by the client the request came over.
    struct ByClient {
        agents: HashMap<&'static str, Vec<Value>>,
        requests: Requests,
        branches: RefCell<VecDeque<BoxFuture<'static, Result<Value, String>>>>,
    }

    impl AgentTransport for ByClient {
        fn agent_request(
            &self,
            client: String,
            command: AgentCommand,
        ) -> BoxFuture<'static, Result<Value, String>> {
            if command.operation == "branch" {
                let answer = self.branches.borrow_mut().pop_front();
                self.requests.borrow_mut().push((client, command));
                return answer.unwrap_or_else(|| {
                    futures::future::ready(Err("no branch answer".into())).boxed()
                });
            }
            let answer = match command.operation.as_str() {
                "snapshot" => Ok(snapshot_answer(
                    &command,
                    self.agents
                        .get(client.as_str())
                        .cloned()
                        .unwrap_or_default(),
                    vec![wire_adapter("wired")],
                )),
                other => Err(format!("{other} is only recorded here")),
            };
            self.requests.borrow_mut().push((client, command));
            futures::future::ready(answer).boxed()
        }
    }

    pub(super) fn agent(id: &str, host: &str, place: (&str, &str, &str, &str)) -> Value {
        let (session, session_name, window, pane) = place;
        let mut agent = wire_agent("blocked", 7, Some("blocked"), 0);
        agent["agentId"] = json!(id);
        let route = &mut agent["route"];
        route["agentId"] = json!(id);
        route["hostProfileId"] = json!(host);
        route["serverIdentity"] = json!("tmux");
        route["sessionId"] = json!(session);
        route["sessionNameFallback"] = json!(session_name);
        route["windowId"] = json!(window);
        route["paneId"] = json!(pane);
        agent
    }

    /// Local, with `notes` pinned, and a second host over ssh showing the
    /// same [`world`].
    pub(super) fn two_hosts(cx: &mut TestAppContext) -> Shell {
        let remote = json!({
            "id": "remote", "label": "Remote",
            "connection": { "mode": "ssh", "profileId": "remote", "target": "remote" },
            "shown": true,
        });
        let mut shell = shell_with(cx, json!([]), json!({}), &[remote]);
        shell.publish_to(
            1,
            vec![frame(6, "connected", &[]), snapshot_frame(1, &world("@1"))],
        );
        // A terminal epoch on each, as a live connection has: agents ask over it.
        let epoch = frame(10, "terminal", &1u64.to_be_bytes());
        shell.publish(vec![epoch.clone()]);
        shell.publish_to(1, vec![epoch]);
        let mut pinned = world("@1");
        pinned.sessions[1].pinned = true;
        shell.publish(vec![snapshot_frame(2, &pinned)]);
        shell
    }

    /// The runtime on the shell's controller, kept in step with it as
    /// `agents::start` keeps it.
    fn start_agents(shell: &mut Shell, agents: HashMap<&'static str, Vec<Value>>) -> Requests {
        start_agents_with(shell, agents, Vec::new()).0
    }

    /// [`start_agents`], with `branches` answered in order and the runtime's
    /// clock handed back.
    pub(super) fn start_agents_with(
        shell: &mut Shell,
        agents: HashMap<&'static str, Vec<Value>>,
        branches: Branches,
    ) -> (Requests, Rc<Effects>) {
        let requests = Requests::default();
        let transport = Rc::new(ByClient {
            agents,
            requests: requests.clone(),
            branches: RefCell::new(branches.into()),
        });
        let clock = Rc::new(Effects::default());
        shell.visual.update(|_, cx| {
            let controller = cx.global::<ConnectionUi>().0.clone();
            let effects = Rc::new(Recorded(clock.clone()));
            let runtime = cx.new(|_| AgentRuntime::new(transport, effects));
            cx.set_global(Agents {
                runtime: runtime.clone(),
                controller: controller.clone(),
            });
            cx.observe(&controller, {
                let runtime = runtime.clone();
                move |controller, cx| agents::sync(&controller, &runtime, cx)
            })
            .detach();
            agents::sync(&controller, &runtime, cx);
        });
        shell.settle();
        (requests, clock)
    }

    pub(super) fn active_host(shell: &mut Shell) -> String {
        shell
            .visual
            .update(|_, cx| cx.global::<ConnectionUi>().0.read(cx).active_profile_id())
    }

    fn agent_inputs(shell: &mut Shell) -> agents::runtime::RuntimeInputs {
        shell
            .visual
            .update(|_, cx| agents::inputs(cx.global::<ConnectionUi>().0.read(cx)))
    }

    #[gpui_kit::test]
    fn asks_agents_only_over_a_writable_epoch_and_focuses_the_active_pane(cx: &mut TestAppContext) {
        let mut shell = shell(cx, json!([]), json!({}));
        let unasked = agent_inputs(&mut shell);
        assert!(unasked.scopes.is_empty(), "no terminal epoch yet");
        let focus = &unasked.focus;
        assert_eq!(
            (
                focus.host_profile_id.as_str(),
                focus.session_id.as_str(),
                focus.window_id.as_str(),
                focus.pane_id.as_str()
            ),
            ("local", "$1", "@1", "%1"),
            "the active window's active pane"
        );
        shell.publish(vec![frame(10, "terminal", &1u64.to_be_bytes())]);
        let live = agent_inputs(&mut shell);
        assert_eq!(live.scopes.len(), 1);
        assert_eq!(live.scopes[0].connection_epoch, 1);
        assert_eq!(live.scopes[0].server_identity, "tmux");
        assert_eq!(live.shown, ["local"]);
        shell.publish(vec![frame(6, "resyncing", &[])]);
        assert!(
            agent_inputs(&mut shell).scopes.is_empty(),
            "a link that cannot mutate is not asked"
        );
    }

    #[gpui_kit::test]
    fn a_launch_opens_under_the_explorers_root_and_waits_for_one(cx: &mut TestAppContext) {
        let mut shell = shell(cx, json!([]), json!({}));
        start_agents(&mut shell, HashMap::new());
        let request = |shell: &mut Shell| {
            shell.visual.update(|_, cx| {
                agents::view::launch_request("claude-code", agents::model::Placement::Split, cx)
            })
        };
        assert!(request(&mut shell).is_none(), "no root, no launch");
        shell.visual.update(|_, cx| {
            cx.set_global(agents::view::ActiveRoot {
                path: "/repo".into(),
                token: "root".into(),
                git_worktree: false,
            })
        });
        let launch = request(&mut shell).unwrap();
        assert_eq!(
            (launch.active_root.as_str(), launch.root_token.as_str()),
            ("/repo", "root")
        );
        assert_eq!(
            (
                launch.session_id.as_str(),
                launch.window_id.as_str(),
                launch.pane_id.as_str()
            ),
            ("$1", "@1", "%1")
        );
    }

    #[gpui_kit::test]
    fn selecting_a_terminal_tab_acknowledges_its_active_pane(cx: &mut TestAppContext) {
        let mut shell = shell(cx, json!([]), json!({}));
        let noted = Rc::new(RefCell::new(Vec::<(String, String)>::new()));
        shell.visual.update(|_, cx| {
            let noted = noted.clone();
            cx.set_global(PaneActivity(Rc::new(move |host, pane, _| {
                noted.borrow_mut().push((host.to_owned(), pane.to_owned()))
            })));
        });
        let workspaces = shell.workspaces();
        shell.visual.update(|_, cx| {
            workspaces.update(cx, |this, cx| this.select_tab(&terminal_key("@2"), cx))
        });
        shell.settle();
        assert_eq!(*noted.borrow(), [("local".to_owned(), "%3".to_owned())]);
    }

    #[gpui_kit::test]
    fn the_agents_list_follows_the_sidebar_order_filter_and_host_letters(cx: &mut TestAppContext) {
        let mut shell = two_hosts(cx);
        start_agents(
            &mut shell,
            HashMap::from([
                (
                    "client-1",
                    vec![
                        agent("in-main", "local", ("$1", "main", "@2", "%3")),
                        agent("in-notes", "local", ("$2", "notes", "@3", "%4")),
                    ],
                ),
                (
                    "client-2",
                    vec![agent("on-remote", "remote", ("$1", "main", "@1", "%1"))],
                ),
            ]),
        );
        let workspaces = shell.workspaces();
        let listed = |shell: &mut Shell, pinned_only: bool| {
            shell.visual.update(|_, cx| {
                agents::rows(cx, AgentSortMode::Workspace, pinned_only)
                    .1
                    .into_iter()
                    .map(|row| {
                        (
                            row.agent.id,
                            row.location.host_letter,
                            row.location.host_label,
                        )
                    })
                    .collect::<Vec<_>>()
            })
        };
        let sidebar = shell.visual.update(|_, cx| {
            workspaces
                .read(cx)
                .rows(false, cx)
                .into_iter()
                .map(|row| (row.host_profile_id, row.session.id))
                .collect::<Vec<_>>()
        });
        let row = |id: &str, letter: &str, label: &str| {
            (id.to_owned(), letter.to_owned(), label.to_owned())
        };
        // The pinned workspace first, as the sidebar draws it.
        assert_eq!(
            sidebar[..3],
            [
                ("local".to_owned(), "$2".to_owned()),
                ("local".to_owned(), "$1".to_owned()),
                ("remote".to_owned(), "$1".to_owned()),
            ]
        );
        assert_eq!(
            listed(&mut shell, false),
            [
                row("in-notes", "L", "Local"),
                row("in-main", "L", "Local"),
                row("on-remote", "R", "Remote"),
            ]
        );
        // Pinned only: the pinned workspace and the one on screen.
        shell
            .visual
            .update(|_, cx| workspaces.update(cx, |this, cx| this.set_pinned_only(true, cx)));
        assert_eq!(
            listed(&mut shell, true),
            [row("in-notes", "L", "Local"), row("in-main", "L", "Local")]
        );
    }

    #[gpui_kit::test]
    fn opening_an_agents_pane_is_the_shells_move_on_either_host(cx: &mut TestAppContext) {
        let mut shell = two_hosts(cx);
        start_agents(&mut shell, HashMap::new());
        shell
            .visual
            .update(|_, cx| agents::open_pane("local", "%3", cx))
            .unwrap();
        shell.settle();
        let focus = shell.host.last("focusPane");
        assert_eq!(
            (focus["pane_id"].as_str(), focus["client"].as_str()),
            (Some("%3"), Some("client-1"))
        );
        // Another workspace whose window tmux says is current: still selected,
        // since focusing a pane leaves its workspace on whatever window it shows.
        shell
            .visual
            .update(|_, cx| agents::open_pane("local", "%4", cx))
            .unwrap();
        shell.settle();
        assert_eq!(shell.host.last("selectWindow")["window_id"], "@3");
        assert_eq!(shell.host.last("focusPane")["pane_id"], "%4");

        shell
            .visual
            .update(|_, cx| agents::open_pane("remote", "%2", cx))
            .unwrap();
        shell.settle();
        assert_eq!(active_host(&mut shell), "remote");
        let focus = shell.host.last("focusPane");
        assert_eq!(
            (focus["pane_id"].as_str(), focus["client"].as_str()),
            (Some("%2"), Some("client-2"))
        );

        let gone = shell
            .visual
            .update(|_, cx| agents::open_pane("remote", "%7", cx));
        assert!(gone.is_err());
    }

    #[gpui_kit::test]
    fn each_agent_row_is_a_tab_stop_walked_by_arrows_with_a_menu(cx: &mut TestAppContext) {
        let mut shell = two_hosts(cx);
        start_agents(
            &mut shell,
            HashMap::from([(
                "client-1",
                vec![
                    agent("in-main", "local", ("$1", "main", "@2", "%3")),
                    agent("in-notes", "local", ("$2", "notes", "@3", "%4")),
                ],
            )]),
        );
        let workspaces = shell.workspaces();
        shell
            .visual
            .update(|window, cx| workspaces.update(cx, |this, cx| this.focus_list(window, cx)));
        shell.settle();
        let on_list = |shell: &mut Shell| focused_row(shell, "agent-row-");
        let reached = (0..12).any(|_| {
            shell.visual.simulate_keystrokes("tab");
            shell.settle();
            on_list(&mut shell).is_some()
        });
        assert!(reached, "Tab reaches the agents list");
        let first = on_list(&mut shell);

        // in-notes, then in-main, in the sidebar's order.
        shell.visual.simulate_keystrokes("down");
        shell.settle();
        let second = on_list(&mut shell);
        assert!(
            second.is_some() && second != first,
            "Down moves to the next row"
        );
        shell.visual.simulate_keystrokes("shift-f10");
        shell.settle();
        let open = shell
            .visual
            .update(|window, cx| crate::context_menu::is_open(window, cx));
        assert!(open, "the menu key opens the row's menu");
        shell.visual.simulate_keystrokes("escape");
        shell.settle();
        assert_eq!(on_list(&mut shell), second, "and hands the keyboard back");
        shell.visual.update(|window, cx| window.press("enter", cx));
        shell.settle();
        assert_eq!(shell.host.last("focusPane")["pane_id"], "%3");
        assert_eq!(
            on_list(&mut shell),
            None,
            "the keyboard goes to the terminal"
        );
    }

    #[gpui_kit::test]
    fn an_agent_on_another_host_whose_pane_is_gone_opens_its_workspace(cx: &mut TestAppContext) {
        let mut shell = two_hosts(cx);
        start_agents(
            &mut shell,
            HashMap::from([(
                "client-2",
                vec![agent("gone", "remote", ("$2", "notes", "@3", "%9"))],
            )]),
        );
        let statuses = Rc::new(RefCell::new(Vec::<String>::new()));
        shell.visual.update(|_, cx| {
            let controller = cx.global::<ConnectionUi>().0.clone();
            let log = statuses.clone();
            cx.subscribe(
                &controller,
                move |_, event: &crate::connection_ui::controller::ConnectionEvent, _| {
                    if let crate::connection_ui::controller::ConnectionEvent::Status(message) =
                        event
                    {
                        log.borrow_mut().push(message.clone());
                    }
                },
            )
            .detach();
            let (all, _) = agents::rows(cx, AgentSortMode::Workspace, false);
            let row = all.iter().find(|row| row.agent.id == "gone").unwrap();
            agents::open_row(row, cx);
        });
        shell.settle();
        assert_eq!(active_host(&mut shell), "remote");
        assert!(
            statuses
                .borrow()
                .iter()
                .any(|status| status.ends_with("; opening its workspace.")),
            "{:?}",
            statuses.borrow()
        );
        assert_eq!(shell.host.last("selectSession")["session_id"], "$2");
    }

    /// The agents feature follows the controller ahead of the shell, so a
    /// click that brings another host on screen starts its move before the
    /// shell has seen the switch. That move belongs to the host now shown and
    /// must keep its last step, the pane itself.
    #[gpui_kit::test]
    fn a_notification_click_from_another_host_reaches_its_pane(cx: &mut TestAppContext) {
        let mut shell = two_hosts(cx);
        start_agents(&mut shell, HashMap::new());
        shell
            .visual
            .update(|_, cx| agents::open_pane("remote", "%4", cx))
            .unwrap();
        shell.settle();
        assert_eq!(active_host(&mut shell), "remote");
        let route = Route {
            host_profile: "local".into(),
            server_identity: "tmux".into(),
            session_id: "$1".into(),
            session_name: "main".into(),
            window_id: "@1".into(),
            window_name: "editor".into(),
            pane_id: "%2".into(),
            agent_id: "waiting".into(),
            attention_generation: 7,
        };
        shell.visual.update(|_, cx| {
            activation::activate(route, false, cx);
            // As `agents::start`'s observer does, before the shell's runs.
            let agents = cx.global::<Agents>();
            let (controller, runtime) = (agents.controller.clone(), agents.runtime.clone());
            agents::sync(&controller, &runtime, cx);
        });
        shell.settle();
        assert_eq!(active_host(&mut shell), "local");
        let focus = shell.host.last("focusPane");
        assert_eq!(
            (focus["pane_id"].as_str(), focus["client"].as_str()),
            (Some("%2"), Some("client-1"))
        );
    }

    #[gpui_kit::test]
    fn a_notification_click_on_another_host_is_the_shells_move_and_acknowledges_on_arrival(
        cx: &mut TestAppContext,
    ) {
        let mut shell = two_hosts(cx);
        let requests = start_agents(
            &mut shell,
            HashMap::from([(
                "client-2",
                vec![agent("on-remote", "remote", ("$1", "main", "@2", "%3"))],
            )]),
        );
        let route = Route {
            host_profile: "remote".into(),
            server_identity: "tmux".into(),
            session_id: "$1".into(),
            session_name: "main".into(),
            window_id: "@2".into(),
            window_name: "server".into(),
            pane_id: "%3".into(),
            agent_id: "on-remote".into(),
            attention_generation: 7,
        };
        shell
            .visual
            .update(|_, cx| activation::activate(route, false, cx));
        shell.settle();
        assert_eq!(active_host(&mut shell), "remote");
        let focus = shell.host.last("focusPane");
        assert_eq!(
            (focus["pane_id"].as_str(), focus["client"].as_str()),
            (Some("%3"), Some("client-2"))
        );
        let seen = |requests: &Requests| {
            requests
                .borrow()
                .iter()
                .filter(|(_, command)| command.operation == "markSeen")
                .map(|(client, command)| {
                    (
                        client.clone(),
                        command.agent_id.clone(),
                        command.attention_generation.clone(),
                    )
                })
                .collect::<Vec<_>>()
        };
        assert!(
            seen(&requests).is_empty(),
            "not before the pane is on screen"
        );
        // The host reports the move; the next poll sees it and acknowledges.
        shell.publish_to(1, vec![snapshot_frame(2, &world("@2"))]);
        shell
            .visual
            .executor()
            .advance_clock(Duration::from_millis(250));
        shell.settle();
        assert_eq!(
            seen(&requests),
            [(
                "client-2".to_owned(),
                "on-remote".to_owned(),
                "7".to_owned()
            )]
        );
    }
}

// Moving by drag and from an agent's menu. Ported from `app/usePaneMoves.test.ts`
// and `features/dragDrop/dropRules.test.ts`, with the gestures made on the real
// elements.

impl Shell {
    fn center(&mut self, label: &str) -> gpui_kit::Point<gpui_kit::Pixels> {
        self.visual
            .update(|window, _| window.find_by_label(label).bounds().center())
    }

    fn drag(
        &mut self,
        from: gpui_kit::Point<gpui_kit::Pixels>,
        to: gpui_kit::Point<gpui_kit::Pixels>,
    ) {
        self.visual.update(|window, cx| window.drag(from, to, cx));
        self.settle();
    }

    fn count(&self, kind: &str) -> usize {
        self.host
            .kinds()
            .iter()
            .filter(|seen| *seen == kind)
            .count()
    }

    fn moves(&mut self, host: &str, pane: &str) -> Option<super::moves::AgentMoves> {
        let workspaces = self.workspaces();
        self.visual
            .update(|_, cx| workspaces.read(cx).agent_moves(host, pane, cx))
    }
}

fn landed(session: &str, window: &str, pane: &str) -> Result<Value, String> {
    Ok(json!({
        "sessionId": session, "windowId": window, "paneId": pane,
        "topologyGeneration": 150,
    }))
}

#[gpui_kit::test]
fn a_tab_dropped_on_a_workspace_moves_its_window_and_leaves_the_view_alone(
    cx: &mut TestAppContext,
) {
    let mut shell = shell(cx, json!([]), json!({}));
    let (from, to) = (shell.center("server"), shell.center("notes"));
    shell.drag(from, to);
    let moved = shell.host.last("moveWindow");
    assert_eq!(moved["window_id"], "@2");
    assert_eq!(moved["target_session_id"], "$2");
    // A move of anything but the pane on screen leaves the view alone.
    assert_eq!(shell.count("selectSession"), 0);
    assert_eq!(shell.selected("editor"), Some(true));
}

#[gpui_kit::test]
fn the_window_on_screen_is_followed_to_where_the_host_says_it_landed(cx: &mut TestAppContext) {
    let mut shell = shell(cx, json!([]), json!({}));
    shell
        .host
        .state()
        .answers
        .push_back(landed("$2", "@1", "%1"));
    let (from, to) = (shell.center("editor"), shell.center("notes"));
    shell.drag(from, to);
    assert_eq!(shell.host.last("moveWindow")["window_id"], "@1");
    assert_eq!(shell.host.last("selectSession")["session_id"], "$2");
}

#[gpui_kit::test]
fn a_document_covering_the_terminal_keeps_the_view_when_the_window_moves(cx: &mut TestAppContext) {
    let tabs = json!([doc("a", "main.rs", 0)]);
    let mut shell = shell(cx, tabs, json!({ "tabsInTitleBar": false }));
    shell.click("workspace-tab-app:a");
    shell
        .host
        .state()
        .answers
        .push_back(landed("$2", "@1", "%1"));
    let (from, to) = (shell.center("editor"), shell.center("notes"));
    shell.drag(from, to);
    assert_eq!(shell.host.last("moveWindow")["window_id"], "@1");
    assert_eq!(shell.count("selectSession"), 0);
    assert_eq!(shell.selected("main.rs"), Some(true));
}

#[gpui_kit::test]
fn a_drop_that_would_do_nothing_or_is_refused_sends_nothing(cx: &mut TestAppContext) {
    let mut shell = shell(cx, json!([]), json!({}));
    // Its own workspace.
    let (from, to) = (shell.center("server"), shell.center("main"));
    shell.drag(from, to);
    assert_eq!(shell.count("moveWindow"), 0);
    // The only tab of `notes` would close it: refused before the host is asked.
    shell
        .visual
        .update(|window, cx| window.click(("workspace-row", 1usize), cx));
    shell.settle();
    let (from, to) = (shell.center("shell"), shell.center("main"));
    shell.drag(from, to);
    assert_eq!(shell.count("moveWindow"), 0);
    let said = shell
        .visual
        .update(|_, cx| cx.global::<ConnectionUi>().0.read(cx).status().to_owned());
    assert_eq!(said, crate::dragdrop::rules::CLOSES_WORKSPACE);
}

#[gpui_kit::test]
fn a_press_that_does_not_travel_is_still_a_click(cx: &mut TestAppContext) {
    let mut shell = shell(cx, json!([]), json!({}));
    shell.click("workspace-tab-terminal:@2");
    assert_eq!(shell.selected("server"), Some(true));
    assert_eq!(shell.count("moveWindow") + shell.count("reorderWindow"), 0);
}

#[gpui_kit::test]
fn a_document_dropped_before_another_reorders_the_strip(cx: &mut TestAppContext) {
    let tabs = json!([doc("a", "main.rs", 0), doc("b", "lib.rs", 1)]);
    let mut shell = shell(cx, tabs, json!({ "tabsInTitleBar": false }));
    let from = shell.center("lib.rs");
    let target = shell
        .visual
        .update(|window, _| window.find_by_label("main.rs").bounds());
    // The near edge of its tab is the gap before it.
    let to = gpui_kit::point(target.left() + px(6.), target.center().y);
    shell.drag(from, to);
    let order: Vec<String> = {
        let workspaces = shell.workspaces();
        shell.visual.update(|_, cx| {
            workspaces
                .read(cx)
                .view(cx)
                .app_tabs
                .iter()
                .map(|tab| tab.id.clone())
                .collect()
        })
    };
    assert_eq!(order, ["b", "a"]);
    // A document's move is the app's own: nothing goes to tmux.
    assert!(shell.host.kinds().iter().all(|kind| !kind.contains("ove")));
}

#[gpui_kit::test]
fn a_document_dropped_at_an_edge_docks_beside_the_terminal(cx: &mut TestAppContext) {
    let tabs = json!([doc("a", "main.rs", 0)]);
    let mut shell = shell(cx, tabs, json!({ "tabsInTitleBar": false }));
    let from = shell.center("main.rs");
    shell.drag(from, gpui_kit::point(px(1270.), px(400.)));
    let workspaces = shell.workspaces();
    let docked = shell.visual.update(|_, cx| {
        workspaces
            .read(cx)
            .view(cx)
            .docked
            .map(|(tab, side)| (tab.id, side))
    });
    assert_eq!(
        docked,
        Some(("a".to_owned(), desktop_core::app_state::DockSide::Right))
    );
}

#[gpui_kit::test]
fn a_pane_is_named_for_its_tab_and_only_the_host_on_screen_can_move_it(cx: &mut TestAppContext) {
    let mut shell = shell(cx, json!([]), json!({}));
    let workspaces = shell.workspaces();
    let label = |shell: &mut Shell, host: &str| {
        let workspaces = workspaces.clone();
        let host = host.to_owned();
        shell.visual.update(|_, cx| {
            workspaces
                .read(cx)
                .pane_payload(&host, "%1", None, cx)
                .map(|payload| payload.label().to_owned())
        })
    };
    assert_eq!(label(&mut shell, "local").as_deref(), Some("editor"));
    assert_eq!(label(&mut shell, "elsewhere"), None);
}

#[gpui_kit::test]
fn an_agent_is_brought_here_from_another_tab_only(cx: &mut TestAppContext) {
    let mut shell = shell(cx, json!([]), json!({}));
    // `%3` is in the tab beside the one on screen; `%2` is already beside `%1`.
    let from_another = shell.moves("local", "%3").expect("on the host on screen");
    assert!(from_another.bring_here.is_some());
    let beside = shell.moves("local", "%2").expect("on the host on screen");
    assert!(beside.bring_here.is_none());
    assert!(shell.moves("elsewhere", "%3").is_none());
}

#[gpui_kit::test]
fn an_agents_own_workspace_is_offered_only_to_split_it_into_a_tab(cx: &mut TestAppContext) {
    let mut shell = shell(cx, json!([]), json!({}));
    // Sharing a tab: it can leave as a tab of its own, here or in `notes`.
    let sharing = shell.moves("local", "%2").unwrap();
    let names: Vec<&str> = sharing
        .workspaces
        .iter()
        .map(|(_, name)| name.as_str())
        .collect();
    assert_eq!(names, ["main (as its own tab)", "notes"]);
    // Alone in its tab: moving it within its workspace would change nothing.
    let alone = shell.moves("local", "%3").unwrap();
    let names: Vec<&str> = alone
        .workspaces
        .iter()
        .map(|(_, name)| name.as_str())
        .collect();
    assert_eq!(names, ["notes"]);
}

#[gpui_kit::test]
fn an_agent_moved_from_its_menu_goes_to_the_chosen_workspace(cx: &mut TestAppContext) {
    let mut shell = shell(cx, json!([]), json!({}));
    let target = shell.moves("local", "%3").unwrap().workspaces[0].0.clone();
    let workspaces = shell.workspaces();
    shell.visual.update(|_, cx| {
        workspaces.update(cx, |this, cx| {
            this.move_agent_to_workspace("local", "%3", target, cx)
        })
    });
    shell.settle();
    let moved = shell.host.last("moveWindow");
    assert_eq!(moved["window_id"], "@2");
    assert_eq!(moved["target_session_id"], "$2");
}

#[gpui_kit::test]
fn bringing_an_agent_here_splits_it_beside_the_pane_on_screen(cx: &mut TestAppContext) {
    let mut shell = shell(cx, json!([]), json!({}));
    let workspaces = shell.workspaces();
    shell
        .visual
        .update(|_, cx| workspaces.update(cx, |this, cx| this.bring_agent_here("local", "%3", cx)));
    shell.settle();
    let joined = shell.host.last("joinPane");
    assert_eq!(joined["pane_id"], "%3");
    assert_eq!(joined["target_pane_id"], "%1");
    assert_eq!(joined["pane_edge"], "right");
}

#[test]
fn only_a_move_of_the_pane_on_screen_is_followed() {
    use super::actions::{ActionKind, TmuxAction};
    use super::moves::pane_on_screen_moved_by;
    let active = Pane {
        active: true,
        ..pane("%1", "@1", "$1", (0, 0, 40, 24))
    };
    let moved = |kind, window: &str, pane: &str| TmuxAction {
        window_id: window.to_owned(),
        pane_id: pane.to_owned(),
        ..TmuxAction::new(kind)
    };
    assert!(
        pane_on_screen_moved_by(&moved(ActionKind::MoveWindow, "@1", ""), Some(&active)).is_some()
    );
    assert!(
        pane_on_screen_moved_by(&moved(ActionKind::MoveWindow, "@2", ""), Some(&active)).is_none()
    );
    assert!(
        pane_on_screen_moved_by(&moved(ActionKind::BreakPane, "", "%1"), Some(&active)).is_some()
    );
    assert!(
        pane_on_screen_moved_by(&moved(ActionKind::BreakPane, "", "%2"), Some(&active)).is_none()
    );
    // The host keeps focus on the pane it had inside a window it joins.
    assert!(
        pane_on_screen_moved_by(&moved(ActionKind::JoinPane, "@1", "%1"), Some(&active)).is_none()
    );
    assert!(pane_on_screen_moved_by(&moved(ActionKind::MoveWindow, "@1", ""), None).is_none());
}

#[gpui_kit::test]
fn escape_ends_a_drag_and_the_release_after_it_drops_nothing(cx: &mut TestAppContext) {
    use gpui_kit::{Modifiers, MouseButton};
    let mut shell = shell(cx, json!([]), json!({}));
    let (from, over) = (shell.center("server"), shell.center("notes"));
    let active = |shell: &mut Shell| shell.visual.update(|_, cx| crate::dragdrop::active(cx));
    shell
        .visual
        .simulate_mouse_down(from, MouseButton::Left, Modifiers::default());
    assert!(!active(&mut shell), "a press alone is no drag");
    shell
        .visual
        .simulate_mouse_move(over, Some(MouseButton::Left), Modifiers::default());
    assert!(active(&mut shell));
    shell.visual.update(|window, cx| window.press("escape", cx));
    assert!(!active(&mut shell));
    shell
        .visual
        .simulate_mouse_up(over, MouseButton::Left, Modifiers::default());
    shell.settle();
    assert_eq!(shell.count("moveWindow"), 0);
    assert_eq!(shell.selected("notes"), Some(false), "and it was no click");
}

#[gpui_kit::test]
fn a_pane_grabbed_with_the_link_modifier_joins_beside_another_pane(cx: &mut TestAppContext) {
    use gpui_kit::{Modifiers, MouseButton};
    let mut shell = shell(cx, json!([]), json!({}));
    let surface = shell
        .visual
        .update(|window, _| window.find_by_label("Workspace main").bounds());
    // `%2` is the right half of the window and `%1` the left.
    let from = gpui_kit::point(
        surface.left() + surface.size.width * 0.8,
        surface.center().y,
    );
    let to = gpui_kit::point(surface.left() + px(14.), surface.center().y);
    let modifier = if cfg!(target_os = "macos") {
        Modifiers::command()
    } else {
        Modifiers::control()
    };
    shell
        .visual
        .simulate_mouse_down(from, MouseButton::Left, modifier);
    shell
        .visual
        .simulate_mouse_move(to, Some(MouseButton::Left), modifier);
    shell
        .visual
        .simulate_mouse_up(to, MouseButton::Left, modifier);
    shell.settle();
    let joined = shell.host.last("joinPane");
    assert_eq!(joined["pane_id"], "%2");
    assert_eq!(joined["target_pane_id"], "%1");
    assert_eq!(joined["pane_edge"], "left");
}

#[gpui_kit::test]
fn a_tab_dropped_back_on_itself_is_not_clicked(cx: &mut TestAppContext) {
    let mut shell = shell(cx, json!([]), json!({}));
    let tab = shell.center("server");
    let beside = gpui_kit::point(tab.x + px(8.), tab.y);
    shell.drag(tab, beside);
    assert_eq!(shell.selected("editor"), Some(true));
    assert_eq!(shell.selected("server"), Some(false));
    assert_eq!(shell.count("moveWindow") + shell.count("reorderWindow"), 0);
}

// Commands against a named target, from `useShellCommands.test.tsx`, and the
// shell over two hosts, from `App.multiHost.test.tsx`.

impl Shell {
    fn status(&mut self) -> String {
        self.visual
            .update(|_, cx| cx.global::<ConnectionUi>().0.read(cx).status().to_owned())
    }

    fn scope(&mut self) -> crate::connection_ui::host_scope::HostScopeToken {
        let workspaces = self.workspaces();
        self.visual
            .update(|_, cx| workspaces.read(cx).view(cx).scope.expect("connected"))
    }

    fn run_on(&mut self, id: &'static str, target: super::Target) {
        let workspaces = self.workspaces();
        self.visual
            .update(|window, cx| super::run_command(&workspaces, id, Some(target), window, cx));
        self.settle();
    }

    fn dialog_open(&mut self) -> bool {
        use gpui_kit::component::WindowExt;
        self.visual
            .update(|window, cx| window.has_active_dialog(cx))
    }

    fn value(&mut self, label: &str) -> Option<String> {
        self.visual
            .update(|window, _| window.find_by_label(label).value().map(str::to_owned))
    }

    /// Replaces what the focused field holds and submits it.
    fn answer(&mut self, text: &str) {
        self.visual.update(|window, cx| {
            window.press("secondary-a", cx);
            window.input(text, cx);
            window.press("enter", cx);
        });
        self.settle();
    }

    /// A click on whatever carries `label`, as a menu item that has no id.
    fn click_label(&mut self, label: &str) {
        use gpui_kit::{Modifiers, MouseButton};
        let at = self.center(label);
        self.visual
            .simulate_mouse_move(at, None, Modifiers::default());
        self.visual
            .simulate_mouse_down(at, MouseButton::Left, Modifiers::default());
        self.visual
            .simulate_mouse_up(at, MouseButton::Left, Modifiers::default());
        self.settle();
    }

    /// How many closes and renames reached a host.
    fn acted(&self) -> usize {
        [
            "closeWindow",
            "closePane",
            "closeSession",
            "renameSession",
            "renameWindow",
        ]
        .into_iter()
        .map(|kind| self.count(kind))
        .sum()
    }

    fn app_tab_ids(&mut self) -> Vec<String> {
        let workspaces = self.workspaces();
        self.visual.update(|_, cx| {
            workspaces
                .read(cx)
                .view(cx)
                .app_tabs
                .iter()
                .map(|tab| tab.id.clone())
                .collect()
        })
    }
}

#[gpui_kit::test]
fn a_rename_opens_on_the_name_the_tab_shows(cx: &mut TestAppContext) {
    let mut shell = shell(cx, json!([]), json!({}));
    // ✓ is a ticker frame and ordinary punctuation; the strip hides a leading
    // one either way, and the dialog matches the strip.
    for (generation, (named, shown)) in [
        ("✳ Fix tests", "Fix tests"),
        ("✓ Deploy checklist", "Deploy checklist"),
    ]
    .into_iter()
    .enumerate()
    {
        let mut next = world("@1");
        next.windows[0].name = named.into();
        shell.publish(vec![snapshot_frame(2 + generation as u64, &next)]);
        shell.run("window.rename");
        assert_eq!(shell.value("Tab name").as_deref(), Some(shown));
        shell.visual.update(|window, cx| window.press("escape", cx));
        shell.settle();
    }
}

#[gpui_kit::test]
fn an_explicit_target_is_acted_on_alone(cx: &mut TestAppContext) {
    use super::Target;
    let tabs = json!([doc("a", "main.rs", 0), doc("b", "lib.rs", 1)]);
    let mut shell = shell(cx, tabs, json!({ "tabsInTitleBar": false }));
    let scope = shell.scope();
    // On screen: `@1`, split, with `%1` focused. Each target names something else.
    shell.run_on("window.close", Target::Window(scope.clone(), "@2".into()));
    assert_eq!(shell.host.last("closeWindow")["window_id"], "@2");
    assert_eq!(shell.count("closePane"), 0, "not the focused pane");
    shell.run_on("pane.close", Target::Pane(scope.clone(), "%2".into()));
    assert_eq!(shell.host.last("closePane")["pane_id"], "%2");
    shell.run_on(
        "session.rename",
        Target::Session(scope.clone(), "$2".into()),
    );
    assert_eq!(shell.value("Workspace name").as_deref(), Some("notes"));
    shell.visual.update(|window, cx| window.press("escape", cx));
    shell.settle();
    shell.click("workspace-tab-app:a");
    shell.run_on("window.close", Target::AppTab(scope, "b".into()));
    assert_eq!(shell.app_tab_ids(), ["a"]);
    assert_eq!(shell.selected("main.rs"), Some(true));
}

#[gpui_kit::test]
fn a_stale_explicit_target_never_falls_back_to_what_is_on_screen(cx: &mut TestAppContext) {
    use super::Target;
    let tabs = json!([doc("a", "main.rs", 0)]);
    let mut shell = shell(cx, tabs, json!({ "tabsInTitleBar": false }));
    let scope = shell.scope();
    shell.run_on(
        "window.close",
        Target::Window(scope.clone(), "@missing".into()),
    );
    shell.run_on("pane.close", Target::Pane(scope.clone(), "%missing".into()));
    shell.run_on(
        "session.close",
        Target::Session(scope.clone(), "$missing".into()),
    );
    shell.run_on(
        "session.rename",
        Target::Session(scope.clone(), "$missing".into()),
    );
    assert!(
        !shell.dialog_open(),
        "no confirmation or prompt for nothing"
    );
    shell.click("workspace-tab-app:a");
    shell.run_on("window.close", Target::AppTab(scope, "tab-missing".into()));
    assert_eq!(shell.app_tab_ids(), ["a"]);
    assert_eq!(shell.acted(), 0, "{:?}", shell.host.kinds());
}

/// A target read before its host scope was replaced names ids that may now
/// be another server's: refused, whichever part of the scope moved.
#[gpui_kit::test]
fn a_target_from_a_replaced_host_scope_is_refused(cx: &mut TestAppContext) {
    use super::Target;
    let mut shell = shell(cx, json!([]), json!({}));
    let current = shell.scope();
    let replaced = [
        crate::connection_ui::host_scope::HostScopeToken {
            host_profile_id: "replacement".into(),
            ..current.clone()
        },
        crate::connection_ui::host_scope::HostScopeToken {
            connection_key: "ssh:replacement".into(),
            ..current.clone()
        },
        crate::connection_ui::host_scope::HostScopeToken {
            connection_epoch: current.connection_epoch + 1,
            ..current.clone()
        },
        crate::connection_ui::host_scope::HostScopeToken {
            server_identity: Some("server-b".into()),
            ..current.clone()
        },
    ];
    for stale in replaced {
        shell.run_on("window.close", Target::Window(stale.clone(), "@1".into()));
        // The tab menu's Close Pane, captured before the switch.
        shell.run_on("pane.close", Target::Pane(stale, "%1".into()));
    }
    assert_eq!(shell.acted(), 0, "{:?}", shell.host.kinds());
    // The same ids under the live scope do reach the host.
    shell.run_on("window.close", Target::Window(current.clone(), "@1".into()));
    assert_eq!(shell.count("closeWindow"), 1);
    shell.run_on("pane.close", Target::Pane(current, "%1".into()));
    assert_eq!(shell.acted(), 2, "{:?}", shell.host.kinds());
}

#[gpui_kit::test]
fn a_new_terminal_tab_from_a_document_keeps_the_document(cx: &mut TestAppContext) {
    let mut notes = doc("b", "notes.md", 1);
    notes["kind"] = json!("markdown");
    notes["viewMode"] = json!("split");
    let tabs = json!([doc("a", "main.rs", 0), notes]);
    let mut shell = shell(cx, tabs, json!({ "tabsInTitleBar": false }));
    for tab in ["workspace-tab-app:a", "workspace-tab-app:b"] {
        shell.click(tab);
        shell.run("window.new");
        assert_eq!(shell.host.last("createWindow")["session_id"], "$1");
        assert_eq!(shell.app_tab_ids(), ["a", "b"]);
    }
    assert_eq!(shell.count("createWindow"), 2);
}

#[gpui_kit::test]
fn the_pinned_only_filter_is_set_from_either_palette_command(cx: &mut TestAppContext) {
    let mut shell = shell(cx, json!([]), json!({}));
    let workspaces = shell.workspaces();
    let filter = |shell: &mut Shell| {
        shell.visual.update(|_, cx| {
            (
                workspaces.read(cx).pinned_only,
                cx.global::<commands::Commands>().context().pinned_only,
            )
        })
    };
    shell.run("workspaces.showPinnedOnly");
    assert_eq!(filter(&mut shell), (true, true));
    shell.run("workspaces.showAll");
    assert_eq!(filter(&mut shell), (false, false));
    assert!(shell.host.kinds().is_empty(), "nothing goes to tmux");
}

#[gpui_kit::test]
fn closing_a_file_tab_says_nothing(cx: &mut TestAppContext) {
    let tabs = json!([doc("a", "main.rs", 0)]);
    let mut shell = shell(cx, tabs, json!({ "tabsInTitleBar": false }));
    shell.click("workspace-tab-app:a");
    let before = shell.status();
    let scope = shell.scope();
    shell.run_on("window.close", super::Target::AppTab(scope, "a".into()));
    assert!(shell.app_tab_ids().is_empty());
    assert_eq!(shell.status(), before, "the tab going is the message");
}

mod multi_host {
    use std::{
        collections::HashMap,
        rc::Rc,
        time::{Duration, Instant},
    };

    use desktop_core::app_state::{AgentSortMode, load_app_state};
    use gpui_kit::Modifiers;

    use super::{
        agent_seams::{Branches, Requests, active_host, agent, start_agents_with, two_hosts},
        *,
    };
    use crate::{agents, commands::RowCommandSource, workspaces::Target};

    fn row(
        shell: &mut Shell,
        host: &str,
        session: &str,
    ) -> (usize, crate::workspaces::rows::WorkspaceRow) {
        let workspaces = shell.workspaces();
        shell.visual.update(|_, cx| {
            workspaces
                .read(cx)
                .rows(true, cx)
                .into_iter()
                .enumerate()
                .find(|(_, row)| row.host_profile_id == host && row.session.id == session)
                .expect("the row is listed")
        })
    }

    fn shown(shell: &mut Shell, host: &str) -> bool {
        shell
            .visual
            .update(|_, cx| cx.global::<ConnectionUi>().0.read(cx).link(host).is_some())
    }

    /// Waits for the New workspace host to reach the saved state: it is
    /// written off the UI thread.
    fn remembered(shell: &mut Shell, expected: &str) {
        let store = shell
            .visual
            .update(|_, cx| cx.global::<crate::app::Core>().app_state.clone());
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let saved = load_app_state(&store)
                .ok()
                .and_then(|state| state.shell.new_workspace_host_profile_id);
            if saved.as_deref() == Some(expected) {
                return;
            }
            assert!(Instant::now() < deadline, "remembered {saved:?}");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[gpui_kit::test]
    fn the_active_hosts_menu_item_cannot_hide_it(cx: &mut TestAppContext) {
        let mut shell = two_hosts(cx);
        shell.click("host-row-button");
        shell.click_label("L Local");
        assert!(shown(&mut shell, "local"));
        assert_eq!(active_host(&mut shell), "local");
        // Only the peer's item toggles. A disabled item may leave the menu up.
        if !shell.shows("R Remote") {
            shell.click("host-row-button");
        }
        shell.click_label("R Remote");
        assert!(!shown(&mut shell, "remote"));
        assert!(shown(&mut shell, "local"));
    }

    fn open_new_workspace_on_remote(shell: &mut Shell) {
        shell.run("session.new");
        assert!(shell.shows("Host: Local"));
        remembered(shell, "local");
        shell.click("new-workspace-host");
        shell.click_label("R Remote");
        assert!(shell.shows("Host: Remote"));
        remembered(shell, "remote");
    }

    #[gpui_kit::test]
    fn a_workspace_is_created_on_the_host_picked_in_the_dialog(cx: &mut TestAppContext) {
        let mut shell = two_hosts(cx);
        shell.visual.update(|window, cx| {
            crate::settings::update(
                |prefs| {
                    prefs.set_workspace_defaults("remote", Some("/srv/work"), Some("git status"))
                },
                window,
                cx,
            );
        });
        open_new_workspace_on_remote(&mut shell);
        shell.click("new-workspace-name");
        shell
            .visual
            .update(|window, cx| window.input("release-checks", cx));
        shell.click("new-workspace-save");
        assert_eq!(active_host(&mut shell), "remote");
        let create = shell.host.last("createSession");
        assert_eq!(create["client"], "client-2");
        assert_eq!(create["name"], "release-checks");
        assert_eq!(create["directory"], "/srv/work");
        assert_eq!(shell.count("createSession"), 1);
        assert_eq!(
            shell.host.state().inputs,
            [("%9".to_owned(), b"git status\n".to_vec())]
        );

        // The pick, not the host on screen, is where the dialog opens next;
        // once that host drops, Save is refused.
        shell.run("session.new");
        assert!(shell.shows("Host: Remote"));
        shell.click("new-workspace-name");
        shell
            .visual
            .update(|window, cx| window.input("must-not-create", cx));
        shell.publish_to(1, vec![frame(6, "disconnected", &[])]);
        let save = shell
            .visual
            .update(|window, _| window.find_by_label("Save").disabled());
        assert_eq!(save, Some(true));
        shell.visual.update(|window, cx| window.press("enter", cx));
        shell.settle();
        assert_eq!(shell.count("createSession"), 1);
    }

    #[gpui_kit::test]
    fn a_host_picked_in_a_cancelled_dialog_is_remembered_but_not_activated(
        cx: &mut TestAppContext,
    ) {
        let mut shell = two_hosts(cx);
        let before = shell.host.kinds();
        open_new_workspace_on_remote(&mut shell);
        shell.click("new-workspace-cancel");
        assert!(!shell.dialog_open());
        assert_eq!(active_host(&mut shell), "local");
        assert_eq!(shell.host.kinds(), before);
        shell.run("session.new");
        assert!(shell.shows("Host: Remote"));
    }

    #[gpui_kit::test]
    fn renaming_a_peers_workspace_goes_to_the_peer(cx: &mut TestAppContext) {
        let mut shell = two_hosts(cx);
        let (_, notes) = row(&mut shell, "remote", "$2");
        shell.run_on("session.rename", Target::Session(notes.scope, "$2".into()));
        assert_eq!(shell.value("Workspace name").as_deref(), Some("notes"));
        shell.answer("notes-renamed");
        let rename = shell.host.last("renameSession");
        assert_eq!(
            (&rename["client"], &rename["session_id"], &rename["name"]),
            (&json!("client-2"), &json!("$2"), &json!("notes-renamed"))
        );
        assert_eq!(active_host(&mut shell), "local");
    }

    #[gpui_kit::test]
    fn closing_a_peers_workspace_asks_and_is_cancelled_if_its_host_went_away(
        cx: &mut TestAppContext,
    ) {
        let mut shell = two_hosts(cx);
        let (_, notes) = row(&mut shell, "remote", "$2");
        shell.run_on("session.close", Target::Session(notes.scope, "$2".into()));
        shell.click("confirmation-confirm");
        let close = shell.host.last("closeSession");
        assert_eq!(
            (&close["client"], &close["session_id"], &close["confirmed"]),
            (&json!("client-2"), &json!("$2"), &json!(true))
        );
        assert_eq!(close["expected_server_identity"], "tmux");

        // The dialog outlives the host: hidden meanwhile, the close has
        // nowhere to go and says so rather than landing on the host on screen.
        let (_, main) = row(&mut shell, "remote", "$1");
        shell.run_on("session.close", Target::Session(main.scope, "$1".into()));
        let workspaces = shell.workspaces();
        shell
            .visual
            .update(|_, cx| workspaces.update(cx, |this, cx| this.toggle_host_shown("remote", cx)));
        shell.settle();
        assert!(!shown(&mut shell, "remote"));
        shell.click("confirmation-confirm");
        assert_eq!(shell.count("closeSession"), 1);
        assert_eq!(
            shell.status(),
            "Closing workspace “main” and all of its windows was cancelled because its host connection changed."
        );
    }

    #[gpui_kit::test]
    fn shift_click_pins_a_peers_workspace_on_the_peer(cx: &mut TestAppContext) {
        let mut shell = two_hosts(cx);
        let (index, _) = row(&mut shell, "remote", "$1");
        shell.visual.update(|window, cx| {
            window.click_with_modifiers(("workspace-row", index), Modifiers::shift(), cx)
        });
        shell.settle();
        let pin = shell.host.last("setPinned");
        assert_eq!(
            (&pin["client"], &pin["session_id"], &pin["pinned"]),
            (&json!("client-2"), &json!("$1"), &json!(true))
        );
        assert_eq!(shell.count("selectSession"), 0);
        assert_eq!(active_host(&mut shell), "local");
    }

    // Agents on the peer.

    fn row_commands(shell: &mut Shell, id: &str) -> Vec<&'static str> {
        with_row(shell, id, |source, _, _| source.available.clone())
    }

    fn run_row(shell: &mut Shell, id: &str, command: &'static str) {
        with_row(shell, id, |source, window, cx| {
            (source.run)(command, window, cx)
        });
    }

    fn with_row<R>(
        shell: &mut Shell,
        id: &str,
        f: impl FnOnce(&RowCommandSource, &mut gpui_kit::Window, &mut gpui_kit::App) -> R,
    ) -> R {
        shell.visual.update(|window, cx| {
            let (all, _) = agents::rows(cx, AgentSortMode::Workspace, false);
            let row = all
                .into_iter()
                .find(|row| row.agent.id == id)
                .expect("the agent is listed");
            let source = agents::view::row_command_source(row, cx);
            f(&source, window, cx)
        })
    }

    #[gpui_kit::test]
    fn a_peer_agents_tab_is_renamed_on_the_peer_and_resume_waits_for_its_host(
        cx: &mut TestAppContext,
    ) {
        let mut shell = two_hosts(cx);
        start_agents_with(
            &mut shell,
            HashMap::from([(
                "client-2",
                vec![agent("on-remote", "remote", ("$1", "main", "@2", "%3"))],
            )]),
            Vec::new(),
        );
        let offered = row_commands(&mut shell, "on-remote");
        assert!(offered.contains(&"agents.renameRow"), "{offered:?}");
        assert!(!offered.contains(&"agents.resumeRow"), "{offered:?}");
        run_row(&mut shell, "on-remote", "agents.resumeRow");
        shell.settle();
        assert_eq!(
            shell.status(),
            "Resume opens a pane on the host on screen; select a workspace on Remote first."
        );

        run_row(&mut shell, "on-remote", "agents.renameRow");
        shell.settle();
        // The tab's name, as the tab shows it.
        assert_eq!(shell.value("Tab name").as_deref(), Some("server"));
        shell.answer("Release notes");
        let rename = shell.host.last("renameWindow");
        assert_eq!(
            (&rename["client"], &rename["window_id"], &rename["name"]),
            (&json!("client-2"), &json!("@2"), &json!("Release notes"))
        );
        assert_eq!(active_host(&mut shell), "local");
    }

    /// On the agent's host, but in a workspace other than the agent's.
    fn on_remote(
        cx: &mut TestAppContext,
        branches: Branches,
    ) -> (Shell, Requests, Rc<crate::agents::tests::Effects>) {
        let mut shell = two_hosts(cx);
        let (requests, clock) = start_agents_with(
            &mut shell,
            HashMap::from([(
                "client-2",
                vec![agent("on-remote", "remote", ("$2", "notes", "@3", "%4"))],
            )]),
            branches,
        );
        shell
            .visual
            .update(|_, cx| agents::open_pane("remote", "%1", cx))
            .unwrap();
        shell.settle();
        assert_eq!(active_host(&mut shell), "remote");
        (shell, requests, clock)
    }

    fn branch(shell: &mut Shell) {
        run_row(shell, "on-remote", "agents.branchRow");
        shell.visual.run_until_parked();
    }

    fn branches(requests: &Requests) -> Vec<(String, String, String)> {
        requests
            .borrow()
            .iter()
            .filter(|(_, command)| command.operation == "branch")
            .map(|(client, command)| {
                (
                    client.clone(),
                    command.session_id.clone(),
                    command.pane_id.clone(),
                )
            })
            .collect()
    }

    fn placed(
        session: &str,
        window: &str,
        pane: &str,
    ) -> BoxFuture<'static, Result<Value, String>> {
        let answer = json!({ "sessionId": session, "windowId": window, "paneId": pane });
        futures::future::ready(Ok(answer)).boxed()
    }

    #[gpui_kit::test]
    fn a_branch_opens_once_its_pane_arrives(cx: &mut TestAppContext) {
        let (mut shell, requests, _) = on_remote(cx, vec![placed("$2", "@7", "%7")]);
        let before = shell.host.kinds().len();
        branch(&mut shell);
        assert_eq!(
            branches(&requests),
            [("client-2".to_owned(), "$2".to_owned(), "%4".to_owned())]
        );
        // The reply beat the snapshot: not gone, just not here yet.
        assert_eq!(shell.host.kinds().len(), before);
        let mut next = world("@1");
        next.windows.push(window("@7", "$2", 1, "branch", false));
        next.panes.push(pane("%7", "@7", "$2", (0, 0, 80, 24)));
        shell.publish_to(1, vec![snapshot_frame(2, &next)]);
        let focus = shell.host.last("focusPane");
        assert_eq!(
            (&focus["client"], &focus["pane_id"]),
            (&json!("client-2"), &json!("%7"))
        );
    }

    #[gpui_kit::test]
    fn a_double_click_sends_one_branch(cx: &mut TestAppContext) {
        let (mut shell, requests, _) =
            on_remote(cx, vec![placed("$2", "@3", "%4"), placed("$2", "@3", "%4")]);
        branch(&mut shell);
        branch(&mut shell);
        assert_eq!(branches(&requests).len(), 1);
    }

    #[gpui_kit::test]
    fn a_branch_whose_pane_never_arrives_says_so_and_stays_put(cx: &mut TestAppContext) {
        let (mut shell, _, clock) = on_remote(cx, vec![placed("$2", "@7", "%7")]);
        let before = shell.host.kinds().len();
        branch(&mut shell);
        let wait = |shell: &mut Shell, ms: u64| {
            clock.now.set(clock.now.get() + ms);
            shell
                .visual
                .executor()
                .advance_clock(Duration::from_millis(ms));
            shell.visual.run_until_parked();
        };
        wait(&mut shell, 4_999);
        assert!(
            !shell.status().contains("has not appeared"),
            "{}",
            shell.status()
        );
        wait(&mut shell, 1);
        let said = shell.status();
        assert!(
            said.starts_with("Branch of ")
                && said
                    .ends_with(" has not appeared. The agent may have no saved conversation yet."),
            "{said}"
        );
        assert_eq!(shell.host.kinds().len(), before);
    }

    #[gpui_kit::test]
    fn a_refused_branch_shows_the_hosts_error_and_moves_nothing(cx: &mut TestAppContext) {
        let refused =
            futures::future::ready(Err("tmux topology generation changed".to_owned())).boxed();
        let (mut shell, _, _) = on_remote(cx, vec![refused]);
        let before = shell.host.kinds().len();
        branch(&mut shell);
        shell.settle();
        assert!(
            shell.status().contains("tmux topology generation changed"),
            "{}",
            shell.status()
        );
        assert_eq!(shell.host.kinds().len(), before);
    }

    #[gpui_kit::test]
    fn a_branch_does_not_pull_back_to_a_host_left_while_it_was_answered(cx: &mut TestAppContext) {
        let (reply, answer) = futures::channel::oneshot::channel::<Result<Value, String>>();
        let held = async move { answer.await.unwrap_or_else(|_| Err("dropped".into())) }.boxed();
        let (mut shell, _, _) = on_remote(cx, vec![held]);
        branch(&mut shell);
        let (index, _) = row(&mut shell, "local", "$1");
        shell
            .visual
            .update(|window, cx| window.click(("workspace-row", index), cx));
        shell.settle();
        assert_eq!(active_host(&mut shell), "local");
        let before = shell.host.kinds().len();
        // A pane that exists: only the user having left keeps it off screen.
        reply
            .send(Ok(
                json!({ "sessionId": "$2", "windowId": "@3", "paneId": "%4" }),
            ))
            .unwrap();
        shell.settle();
        assert_eq!(active_host(&mut shell), "local");
        assert_eq!(shell.host.kinds().len(), before, "{:?}", shell.host.kinds());
    }
}

/// Which of the buttons named `labels` holds the keyboard, if any.
fn focused_button(shell: &mut Shell, labels: &[&'static str]) -> Option<&'static str> {
    shell.visual.update(|window, _| {
        labels.iter().copied().find(|label| {
            window
                .try_find_by_label(label)
                .is_some_and(|element| element.focused() == Some(true))
        })
    })
}

#[gpui_kit::test]
fn tab_reaches_the_title_bar_and_the_strip_and_shift_tab_the_panel_tabs(cx: &mut TestAppContext) {
    let tabs = json!([doc("a", "main.rs", 0), doc("b", "lib.rs", 1)]);
    let mut shell = shell(
        cx,
        tabs,
        json!({ "tabsInTitleBar": false, "panelOpen": true, "panelSurface": "files" }),
    );
    let workspaces = shell.workspaces();
    // The menus are native on macOS: the button is Linux's.
    let title_bar: Vec<&'static str> = [
        "Settings",
        "Toggle sidebar",
        "New workspace",
        "Toggle right panel",
    ]
    .into_iter()
    .chain(cfg!(target_os = "linux").then_some("Main menu"))
    .collect();
    // Back from the workspace list through the title bar, in its order.
    shell
        .visual
        .update(|window, cx| workspaces.update(cx, |this, cx| this.focus_list(window, cx)));
    shell.settle();
    let mut walked = Vec::new();
    for _ in 0..8 {
        shell.visual.simulate_keystrokes("shift-tab");
        shell.settle();
        walked.extend(focused_button(&mut shell, &title_bar));
        if walked.last() == Some(&"Settings") {
            break;
        }
    }
    let mut expected = title_bar.clone();
    expected.reverse();
    assert_eq!(walked, expected);

    // On from the list to the strip, then the documents' close buttons.
    shell
        .visual
        .update(|window, cx| workspaces.update(cx, |this, cx| this.focus_list(window, cx)));
    shell.settle();
    let strip = (0..10).position(|_| {
        shell.visual.simulate_keystrokes("tab");
        shell.settle();
        shell
            .visual
            .update(|window, cx| workspaces.read(cx).strip_focus.is_focused(window))
    });
    assert!(strip.is_some(), "Tab reaches the tab strip");
    shell.visual.simulate_keystrokes("tab");
    shell.settle();
    assert_eq!(
        focused_button(&mut shell, &["Close main.rs"]),
        Some("Close main.rs")
    );

    // Past the start of the window the panel's tabs come, as the last stops in
    // Tauri's page did; the arrows select a surface, wrapping.
    shell
        .visual
        .update(|window, cx| workspaces.update(cx, |this, cx| this.focus_list(window, cx)));
    shell.settle();
    while focused_button(&mut shell, &["Settings"]).is_none() {
        shell.visual.simulate_keystrokes("shift-tab");
        shell.settle();
    }
    let on_tabs = (0..4).any(|_| {
        shell.visual.simulate_keystrokes("shift-tab right");
        shell.settle();
        shell.selected("Git") == Some(true)
    });
    assert!(on_tabs, "Shift+Tab reaches the panel's tabs");
    shell.visual.simulate_keystrokes("left left");
    shell.settle();
    assert_eq!(
        shell.selected("Automations"),
        Some(true),
        "Left wraps to the last"
    );
    shell.visual.simulate_keystrokes("right");
    shell.settle();
    assert_eq!(
        shell.selected("Files"),
        Some(true),
        "Right wraps to the first"
    );
}

#[gpui_kit::test]
fn the_strip_selects_tabs_and_opens_their_menu_from_the_keyboard(cx: &mut TestAppContext) {
    let tabs = json!([doc("a", "main.rs", 0), doc("b", "lib.rs", 1)]);
    let mut shell = shell(cx, tabs, json!({ "tabsInTitleBar": false }));
    let workspaces = shell.workspaces();
    shell
        .visual
        .update(|window, cx| workspaces.update(cx, |this, cx| this.focus_strip(window, cx)));
    shell.settle();
    shell.visual.simulate_keystrokes("end enter");
    shell.settle();
    assert_eq!(shell.selected("lib.rs"), Some(true));
    shell.visual.simulate_keystrokes("home right enter");
    shell.settle();
    assert_eq!(shell.selected("server"), Some(true));
    assert_eq!(shell.selected("lib.rs"), Some(false));

    // Choosing a terminal tab handed the keyboard to its pane.
    shell
        .visual
        .update(|window, cx| workspaces.update(cx, |this, cx| this.focus_strip(window, cx)));
    shell.settle();
    shell.visual.simulate_keystrokes("shift-f10");
    shell.settle();
    let open = shell
        .visual
        .update(|window, cx| crate::context_menu::is_open(window, cx));
    assert!(open, "the menu key opens the tab's menu");
    shell.visual.simulate_keystrokes("escape");
    shell.settle();
    let back = shell
        .visual
        .update(|window, cx| workspaces.read(cx).strip_focus.is_focused(window));
    assert!(
        back,
        "closing the menu hands the keyboard back to the strip"
    );
    // The region is named for the workspace on screen, as in Tauri.
    shell.visual.update(|window, _| {
        assert_eq!(
            window.find_by_label("Workspace main").role(),
            Some(Role::Main)
        );
    });
}

#[gpui_kit::test]
fn a_dialog_from_the_host_menu_hands_the_keyboard_back_to_the_host_button(cx: &mut TestAppContext) {
    let mut shell = shell(cx, json!([]), json!({}));
    let workspaces = shell.workspaces();
    let on_host = |shell: &mut Shell| {
        shell.visual.update(|window, _| {
            window.elements().into_iter().any(|element| {
                element
                    .label()
                    .is_some_and(|label| label.starts_with("Host "))
                    && element.role() == Some(Role::Button)
                    && element.focused() == Some(true)
            })
        })
    };
    shell
        .visual
        .update(|window, cx| workspaces.update(cx, |this, cx| this.focus_list(window, cx)));
    shell.settle();
    let reached = (0..12).any(|_| {
        shell.visual.simulate_keystrokes("tab");
        shell.settle();
        on_host(&mut shell)
    });
    assert!(reached, "Tab reaches the host button");
    shell.visual.update(|window, cx| window.press("enter", cx));
    shell.settle();
    shell.settle();
    // "Connection settings…" is the last item.
    shell.visual.simulate_keystrokes("end enter");
    shell.settle();
    assert!(
        shell
            .visual
            .update(gpui_kit::component::WindowExt::has_active_dialog)
    );
    shell.visual.simulate_keystrokes("escape");
    shell.settle();
    assert!(
        !shell
            .visual
            .update(gpui_kit::component::WindowExt::has_active_dialog)
    );
    assert!(
        on_host(&mut shell),
        "back where the menu opened, as in Tauri"
    );
}

/// Two stops, under Kit's root as the app is.
struct TwoStops([gpui_kit::FocusHandle; 2]);

impl gpui_kit::Render for TwoStops {
    fn render(
        &mut self,
        _: &mut gpui_kit::Window,
        _: &mut gpui_kit::Context<Self>,
    ) -> impl gpui_kit::IntoElement {
        use gpui_kit::{InteractiveElement as _, ParentElement as _, Styled as _};
        gpui_kit::div()
            .size_full()
            .child(gpui_kit::div().id("first").size_4().track_focus(&self.0[0]))
            .child(gpui_kit::div().id("last").size_4().track_focus(&self.0[1]))
    }
}

#[gpui_kit::test]
fn tab_with_the_keyboard_nowhere_goes_to_the_first_stop(cx: &mut TestAppContext) {
    cx.update(|cx| {
        gpui_kit::init(cx);
        crate::ui::chrome::init(cx);
    });
    let stops = cx.new(|cx| TwoStops([0, 1].map(|_| cx.focus_handle().tab_stop(true))));
    let (_, visual) = cx.add_window_view({
        let stops = stops.clone();
        move |window, cx| gpui_kit::base::Root::new(stops, window, cx)
    });
    let focused = |visual: &mut VisualTestContext| {
        visual.update(|window, cx| {
            stops
                .read(cx)
                .0
                .iter()
                .position(|stop| stop.is_focused(window))
        })
    };
    visual.simulate_keystrokes("tab");
    assert_eq!(focused(visual), Some(0));
    visual.update(|window, cx| window.blur(cx));
    visual.simulate_keystrokes("shift-tab");
    assert_eq!(focused(visual), Some(1));
}
