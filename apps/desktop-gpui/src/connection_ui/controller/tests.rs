//! The controller's behaviour, ported from `missingHelperRecovery.test.tsx`,
//! `reconnectNoticeDedupe.test.tsx`, `multiHostBridges.test.tsx` and
//! `helperSetupFeedback.test.tsx`, plus resume rebuilds and profile loading.

use std::{
    collections::VecDeque,
    path::Path,
    sync::{Arc, Mutex},
};

use desktop_core::{
    app_state::{AppStateStore, AppTabKind, AppTabRecord},
    connection::{ConnectionSpec, ProfileStore, helper::HelperRollback},
    platform::{EventBody, EventSink},
};
use futures::{
    channel::oneshot,
    future::{BoxFuture, FutureExt},
};
use gpui_kit::{AppContext, Entity, TestAppContext};
use serde_json::{Value, json};

use super::*;
use crate::connection_ui::{
    backend::{EngineChannel, TerminalBackend, inline::InlineEngine},
    helper_upgrade::RemoteHelperProbe,
};
use crate::terminal::link::POLL_IDLE;

struct Bridge {
    client_id: String,
    connection: ConnectionSpec,
    attach: bool,
    session_id: String,
    sink: Arc<dyn EventSink>,
}

#[derive(Default)]
struct FakeState {
    bridges: Vec<Bridge>,
    start_failure: Option<String>,
    stopped: Vec<String>,
    acks: Vec<(String, u64, u64, u64)>,
    probes: Vec<ConnectionSpec>,
    helper_answers: VecDeque<Result<RemoteHelperProbe, String>>,
    held_answers: Vec<oneshot::Sender<Result<RemoteHelperProbe, String>>>,
    hold_probes: bool,
    installs: Vec<(ConnectionSpec, bool)>,
    install_reports: VecDeque<Result<HelperInstallReport, String>>,
    hold_installs: bool,
    link_probes: VecDeque<Result<(), String>>,
    hold_link_probes: bool,
    discards: u32,
    saved_app_states: Vec<PersistedAppState>,
    save_failure: Option<String>,
    late_requests: Option<u32>,
    incidents: Vec<(String, Value)>,
    now: u64,
    online: bool,
    seeds: Vec<String>,
    prewarms: Vec<String>,
}

#[derive(Clone, Default)]
struct Fake(Arc<Mutex<FakeState>>);

impl Fake {
    fn new() -> Self {
        let fake = Self::default();
        fake.state().online = true;
        fake.state().now = 1_000;
        fake
    }

    fn state(&self) -> std::sync::MutexGuard<'_, FakeState> {
        self.0.lock().unwrap()
    }

    fn publish(&self, index: usize, frame: Vec<u8>) {
        let sink = self.state().bridges[index].sink.clone();
        sink.send(EventBody::Raw(frame)).unwrap();
    }

    fn bridge_count(&self) -> usize {
        self.state().bridges.len()
    }

    fn bridge_index(&self, client_id: &str) -> usize {
        self.state()
            .bridges
            .iter()
            .position(|bridge| bridge.client_id == client_id)
            .unwrap()
    }

    fn incident_kinds(&self) -> Vec<String> {
        self.state()
            .incidents
            .iter()
            .map(|(kind, _)| kind.clone())
            .collect()
    }
}

impl TerminalBackend for Fake {
    fn spawn_engine(
        &self,
        outputs: std::sync::mpsc::Sender<terminal_engine::EngineOutput>,
    ) -> Box<dyn EngineChannel> {
        Box::new(InlineEngine::new(outputs))
    }

    fn request_seed(&self, _: String, pane_id: String) -> BoxFuture<'static, Result<(), String>> {
        self.state().seeds.push(pane_id);
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

    fn send_input(&self, _: String, _: String, _: Vec<u8>) -> Result<(), String> {
        Ok(())
    }

    fn send_paste(&self, _: String, _: String, _: Vec<u8>) -> Result<(), String> {
        Ok(())
    }

    fn record_incident(&self, kind: &str, detail: Value) {
        self.state().incidents.push((kind.to_owned(), detail));
    }

    fn set_terminal_colors(&self, _: String, _: String) -> Result<(), String> {
        Ok(())
    }

    fn tmux_action(&self, _: String, _: Value) -> BoxFuture<'static, Result<Value, String>> {
        async { Ok(json!({})) }.boxed()
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

impl ConnectionBackend for Fake {
    fn start_terminal(
        &self,
        session_id: String,
        connection: ConnectionSpec,
        attach: bool,
        sink: Arc<dyn EventSink>,
    ) -> Result<String, String> {
        let mut state = self.state();
        if let Some(error) = state.start_failure.clone() {
            return Err(error);
        }
        let client_id = format!("client-{}", state.bridges.len() + 1);
        state.bridges.push(Bridge {
            client_id: client_id.clone(),
            connection,
            attach,
            session_id,
            sink,
        });
        Ok(client_id)
    }

    fn stop_terminal(&self, client_id: String) {
        self.state().stopped.push(client_id);
    }

    fn acknowledge_delivery(
        &self,
        client_id: String,
        epoch: u64,
        frames: u64,
        bytes: u64,
    ) -> BoxFuture<'static, Result<(), String>> {
        self.state().acks.push((client_id, epoch, frames, bytes));
        async { Ok(()) }.boxed()
    }

    fn probe_link(&self, _client_id: String) -> BoxFuture<'static, Result<(), String>> {
        let mut state = self.state();
        if state.hold_link_probes {
            return futures::future::pending().boxed();
        }
        let answer = state.link_probes.pop_front().unwrap_or(Ok(()));
        async move { answer }.boxed()
    }

    fn discard_ssh_masters(&self) -> BoxFuture<'static, Result<(), String>> {
        self.state().discards += 1;
        async { Ok(()) }.boxed()
    }

    fn probe_helper(
        &self,
        connection: ConnectionSpec,
    ) -> BoxFuture<'static, Result<RemoteHelperProbe, String>> {
        let mut state = self.state();
        state.probes.push(connection);
        if state.hold_probes {
            let (sender, receiver) = oneshot::channel();
            state.held_answers.push(sender);
            return async move { receiver.await.unwrap_or_else(|_| Err("dropped".into())) }.boxed();
        }
        let answer = state
            .helper_answers
            .pop_front()
            .unwrap_or_else(|| Err("no answer queued".into()));
        async move { answer }.boxed()
    }

    fn install_helper(
        &self,
        connection: ConnectionSpec,
        allow_upgrade: bool,
    ) -> BoxFuture<'static, Result<HelperInstallReport, String>> {
        let mut state = self.state();
        state.installs.push((connection, allow_upgrade));
        if state.hold_installs {
            return futures::future::pending().boxed();
        }
        let report = state.install_reports.pop_front().unwrap_or_else(|| {
            Ok(HelperInstallReport {
                ok: true,
                message: "verified".into(),
                rollback: HelperRollback::NotNeeded,
            })
        });
        async move { report }.boxed()
    }

    fn save_app_state(&self, state: PersistedAppState) -> BoxFuture<'static, Result<(), String>> {
        let mut fake = self.state();
        fake.saved_app_states.push(state);
        let answer = fake.save_failure.clone().map_or(Ok(()), Err);
        async move { answer }.boxed()
    }

    fn prewarm_bulk(&self, client_id: String) {
        self.state().prewarms.push(client_id);
    }

    fn late_requests(&self, _client_id: &str) -> Option<u32> {
        self.state().late_requests
    }

    fn now_ms(&self) -> u64 {
        self.state().now
    }

    fn network_online(&self) -> bool {
        self.state().online
    }
}

fn frame(kind: u8, label: &str, sequence: u64, payload: &[u8]) -> Vec<u8> {
    let mut bytes = vec![kind];
    bytes.extend_from_slice(&(label.len() as u16).to_be_bytes());
    bytes.extend_from_slice(label.as_bytes());
    bytes.extend_from_slice(&sequence.to_be_bytes());
    bytes.extend_from_slice(payload);
    bytes
}

fn state_frame(state: &str) -> Vec<u8> {
    frame(6, state, 0, &[])
}

fn error_frame(message: &str) -> Vec<u8> {
    frame(4, message, 0, &[])
}

fn stall_frame(pane: &str, sequence: u64) -> Vec<u8> {
    frame(15, pane, sequence, b"stalled")
}

fn epoch_frame(epoch: u64) -> Vec<u8> {
    frame(10, "terminal", 0, &epoch.to_be_bytes())
}

fn world(server: &str, generation: u64, sessions: &[(&str, &str)]) -> Vec<u8> {
    let sessions: Vec<Value> = sessions
        .iter()
        .map(|(id, name)| {
            json!({
                "id": id, "name": name, "windowCount": 1, "attachedClients": 0, "order": 0,
            })
        })
        .collect();
    let body = json!({
        "sequence": 0,
        "generation": generation,
        "serverIdentity": server,
        "authoritative": true,
        "snapshot": { "sessions": sessions, "windows": [], "panes": [] },
    });
    frame(7, "snapshot", 0, body.to_string().as_bytes())
}

const HANDSHAKE_FAILURE: &str = "host closed during handshake: No such file or directory";

fn probe_of(installed: bool, compatible: bool) -> RemoteHelperProbe {
    RemoteHelperProbe {
        operating_system: "Linux".into(),
        architecture: "x86_64".into(),
        tmux_version: "tmux 3.3a".into(),
        git_version: "git version 2.39".into(),
        installed,
        helper_version: installed.then(|| "0.0.1".to_owned()),
        compatible,
        digest: None,
        remote_path: "$HOME/.local/bin/muxflow-host".into(),
    }
}

fn ssh_profile(id: &str, target: &str, shown: bool) -> Value {
    json!({
        "id": id,
        "label": target,
        "connection": { "mode": "ssh", "profileId": id, "target": target },
        "shown": shown,
    })
}

fn local_profile(shown: bool) -> Value {
    json!({ "id": "local", "label": "Local", "connection": { "mode": "local" }, "shown": shown })
}

struct Harness {
    fake: Fake,
    controller: Entity<ConnectionController>,
    _dir: tempfile::TempDir,
}

impl Harness {
    fn read<R>(&self, cx: &mut TestAppContext, read: impl FnOnce(&ConnectionController) -> R) -> R {
        cx.read_entity(&self.controller, |state, _| read(state))
    }

    fn update(
        &self,
        cx: &mut TestAppContext,
        change: impl FnOnce(&mut ConnectionController, &mut Context<ConnectionController>),
    ) {
        self.controller.update(cx, change);
        cx.run_until_parked();
    }

    fn publish(&self, cx: &mut TestAppContext, index: usize, frames: Vec<Vec<u8>>) {
        for frame in frames {
            self.fake.publish(index, frame);
        }
        cx.run_until_parked();
        // What the engine produced waits for the link's next drain.
        cx.executor().advance_clock(POLL_IDLE);
        cx.run_until_parked();
    }

    fn helper(&self, cx: &mut TestAppContext) -> HelperUpgradeState {
        self.read(cx, |state| state.helper().clone())
    }

    fn statuses(&self, cx: &mut TestAppContext) -> Arc<Mutex<Vec<String>>> {
        let statuses = Arc::new(Mutex::new(Vec::new()));
        let writer = statuses.clone();
        let controller = self.controller.clone();
        cx.update(|cx| {
            cx.subscribe(&controller, move |_, event: &ConnectionEvent, _| {
                let ConnectionEvent::Status(message) = event else {
                    return;
                };
                writer.lock().unwrap().push(message.clone());
            })
            .detach();
        });
        statuses
    }
}

fn harness_with(
    cx: &mut TestAppContext,
    profiles: Vec<Value>,
    last: &str,
    prepare: impl FnOnce(&Fake),
    prepare_state: impl FnOnce(&Path),
) -> Harness {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("profiles.json");
    let saved = json!({ "schemaVersion": 1, "profiles": profiles, "lastProfileId": last });
    std::fs::write(&path, saved.to_string()).unwrap();
    prepare_state(dir.path());
    let store = Arc::new(ProfileStore::load(path).unwrap());
    let app_state = Arc::new(AppStateStore::load(dir.path().join("app-state.json")));
    let fake = Fake::new();
    prepare(&fake);
    let backend: Arc<dyn ConnectionBackend> = Arc::new(fake.clone());
    let controller = cx.new(|cx| ConnectionController::new(backend, store, app_state, cx));
    cx.run_until_parked();
    Harness {
        fake,
        controller,
        _dir: dir,
    }
}

fn ssh_host(cx: &mut TestAppContext, prepare: impl FnOnce(&Fake)) -> Harness {
    harness_with(
        cx,
        vec![ssh_profile("remote-a", "qa-host", true)],
        "remote-a",
        prepare,
        |_| {},
    )
}

/// Local active, one SSH peer shown beside it.
fn two_shown_hosts(cx: &mut TestAppContext) -> Harness {
    harness_with(
        cx,
        vec![
            local_profile(true),
            ssh_profile("remote-a", "qa-host", true),
        ],
        "local",
        |_| {},
        |_| {},
    )
}

// Saved profiles and the first bridge.

#[gpui_kit::test]
fn a_saved_profile_from_the_tauri_file_is_the_first_bridge(cx: &mut TestAppContext) {
    let harness = ssh_host(cx, |_| {});
    assert_eq!(harness.fake.bridge_count(), 1);
    let state = harness.fake.state();
    assert_eq!(state.bridges[0].client_id, "client-1");
    assert_eq!(state.bridges[0].session_id, "");
    assert!(state.bridges[0].attach);
    assert_eq!(
        state.bridges[0].connection,
        ConnectionSpec::Ssh {
            profile_id: "remote-a".into(),
            target: "qa-host".into(),
            config_path: None,
        }
    );
    drop(state);
    harness.read(cx, |state| {
        assert_eq!(state.selected_profile_id(), "remote-a");
        assert_eq!(state.connection_mode(), ConnectionMode::Ssh);
        assert_eq!(state.ssh_target(), "qa-host");
    });
}

#[gpui_kit::test]
fn a_local_connect_reports_its_routine_status_and_saves_the_host(cx: &mut TestAppContext) {
    let harness = two_shown_hosts(cx);
    let statuses = harness.statuses(cx);
    harness.update(cx, |state, cx| state.connect(cx));
    assert_eq!(
        statuses.lock().unwrap().as_slice(),
        ["Discovering local tmux…"]
    );
    // The epoch bump replaces the active bridge and nothing else.
    assert_eq!(harness.fake.state().stopped, ["client-1"]);
    assert_eq!(harness.fake.bridge_count(), 3);
}

#[gpui_kit::test]
fn connecting_an_empty_form_only_asks_for_a_host(cx: &mut TestAppContext) {
    let harness = two_shown_hosts(cx);
    let statuses = harness.statuses(cx);
    harness.update(cx, |state, cx| state.add_host(cx));
    harness.update(cx, |state, cx| state.connect(cx));
    assert_eq!(
        statuses.lock().unwrap().as_slice(),
        ["Enter an SSH host or config alias."]
    );
    assert_eq!(harness.fake.bridge_count(), 2);
}

/// Port of the second half of `App.integration.test.tsx`'s "edits the picked
/// host in place, and adds one only when asked to".
#[gpui_kit::test]
fn add_host_starts_an_empty_form_and_connect_saves_a_new_host(cx: &mut TestAppContext) {
    let harness = ssh_host(cx, |_| {});
    harness.update(cx, |state, cx| state.add_host(cx));
    harness.read(cx, |state| {
        assert_eq!(state.ssh_target(), "");
        assert_eq!(state.selected_profile_id(), "");
        assert_eq!(state.connection_mode(), ConnectionMode::Ssh);
    });
    harness.update(cx, |state, cx| {
        state.set_ssh_target("staging-host".into(), cx)
    });
    harness.update(cx, |state, cx| state.connect(cx));
    let path = harness._dir.path().join("profiles.json");
    let saved = list_host_profiles(&ProfileStore::load(path).unwrap()).unwrap();
    let profiles = saved["profiles"].as_array().unwrap();
    assert_eq!(profiles.len(), 2, "{saved}");
    let kept = profiles
        .iter()
        .find(|profile| profile["id"] == "remote-a")
        .expect("the first host is still there");
    assert_eq!(kept["connection"]["target"], "qa-host");
    let added = profiles
        .iter()
        .find(|profile| profile["label"] == "staging-host")
        .expect("the new host was saved");
    assert_ne!(added["id"], "remote-a");
    assert_eq!(added["connection"]["target"], "staging-host");
}

#[gpui_kit::test]
fn correcting_a_picked_hosts_address_moves_it_with_exactly_one_restart(cx: &mut TestAppContext) {
    let harness = two_shown_hosts(cx);
    harness.update(cx, |state, cx| state.select_profile("remote-a", cx));
    harness.update(cx, |state, cx| state.set_ssh_target("qa-host-2".into(), cx));
    harness.update(cx, |state, cx| state.connect(cx));
    assert_eq!(harness.fake.state().stopped, ["client-2"]);
    assert_eq!(harness.fake.bridge_count(), 3);
    harness.read(cx, |state| {
        assert_eq!(state.active_profile_id(), "remote-a");
        assert_eq!(state.links().count(), 2);
        assert_eq!(state.profiles().len(), 2);
        assert_eq!(state.profiles()[1].id, "remote-a");
        assert_eq!(state.profiles()[1].label, "qa-host-2");
    });
    let state = harness.fake.state();
    assert_eq!(state.bridges[2].client_id, "client-3");
    assert!(state.bridges[2].attach);
    assert_eq!(
        state.bridges[2].connection,
        ConnectionSpec::Ssh {
            profile_id: "remote-a".into(),
            target: "qa-host-2".into(),
            config_path: None,
        }
    );
}

// Missing-helper recovery.

fn queue_answer(fake: &Fake, answer: Result<RemoteHelperProbe, String>) {
    fake.state().helper_answers.push_back(answer);
}

fn hold_probes(fake: &Fake) {
    fake.state().hold_probes = true;
}

fn resolve_probe(
    harness: &Harness,
    cx: &mut TestAppContext,
    index: usize,
    answer: RemoteHelperProbe,
) {
    let sender = harness.fake.state().held_answers.remove(index);
    sender.send(Ok(answer)).ok();
    cx.run_until_parked();
}

#[gpui_kit::test]
fn asks_the_host_once_and_opens_the_install(cx: &mut TestAppContext) {
    let harness = ssh_host(cx, |fake| queue_answer(fake, Ok(probe_of(false, false))));
    harness.publish(cx, 0, vec![error_frame(HANDSHAKE_FAILURE)]);
    assert_eq!(harness.fake.state().probes.len(), 1);
    assert!(matches!(
        harness.helper(cx),
        HelperUpgradeState::Confirming {
            operation: HelperOperation::Install,
            ..
        }
    ));
    harness.read(cx, |state| {
        assert!(
            state
                .active_link()
                .unwrap()
                .detail
                .contains("No such file or directory")
        );
    });
    harness.publish(
        cx,
        0,
        vec![
            error_frame(HANDSHAKE_FAILURE),
            error_frame(HANDSHAKE_FAILURE),
        ],
    );
    assert_eq!(harness.fake.state().probes.len(), 1);
}

#[gpui_kit::test]
fn checks_a_successful_connection_once_and_prompts_for_a_digest_mismatch(cx: &mut TestAppContext) {
    let harness = ssh_host(cx, |fake| queue_answer(fake, Ok(probe_of(true, false))));
    harness.publish(cx, 0, vec![state_frame("connected")]);
    assert_eq!(harness.fake.state().probes.len(), 1);
    assert!(matches!(
        harness.helper(cx),
        HelperUpgradeState::Confirming {
            operation: HelperOperation::Upgrade,
            ..
        }
    ));
    harness.update(cx, |state, cx| state.cancel_helper_install(cx));
    harness.publish(
        cx,
        0,
        vec![state_frame("resyncing"), state_frame("connected")],
    );
    assert_eq!(harness.fake.state().probes.len(), 1);
    assert!(matches!(
        harness.helper(cx),
        HelperUpgradeState::Ready { .. }
    ));
}

#[gpui_kit::test]
fn re_probes_an_in_place_reconnect_and_ignores_the_old_answer(cx: &mut TestAppContext) {
    let harness = ssh_host(cx, hold_probes);
    harness.publish(cx, 0, vec![state_frame("connected")]);
    assert!(matches!(
        harness.helper(cx),
        HelperUpgradeState::Probing { .. }
    ));
    harness.publish(
        cx,
        0,
        vec![
            state_frame("disconnected"),
            state_frame("reconnecting"),
            state_frame("connected"),
        ],
    );
    assert_eq!(harness.fake.state().probes.len(), 2);
    resolve_probe(&harness, cx, 0, probe_of(true, false));
    assert!(matches!(
        harness.helper(cx),
        HelperUpgradeState::Probing { .. }
    ));
    resolve_probe(&harness, cx, 0, probe_of(true, true));
    assert!(matches!(
        harness.helper(cx),
        HelperUpgradeState::Ready { .. }
    ));
}

#[gpui_kit::test]
fn withdraws_a_settled_automatic_confirmation_while_its_transport_reconnects(
    cx: &mut TestAppContext,
) {
    let harness = ssh_host(cx, |fake| {
        queue_answer(fake, Ok(probe_of(true, false)));
        queue_answer(fake, Ok(probe_of(true, false)));
    });
    harness.publish(cx, 0, vec![state_frame("connected")]);
    assert!(matches!(
        harness.helper(cx),
        HelperUpgradeState::Confirming { .. }
    ));
    harness.publish(cx, 0, vec![state_frame("disconnected")]);
    assert_eq!(harness.helper(cx), HelperUpgradeState::Idle);
    harness.publish(
        cx,
        0,
        vec![state_frame("reconnecting"), state_frame("connected")],
    );
    assert_eq!(harness.fake.state().probes.len(), 2);
    assert!(matches!(
        harness.helper(cx),
        HelperUpgradeState::Confirming { .. }
    ));
}

#[gpui_kit::test]
fn a_coalesced_settings_check_cannot_weaken_the_automatic_decision(cx: &mut TestAppContext) {
    let harness = ssh_host(cx, hold_probes);
    harness.publish(cx, 0, vec![state_frame("connected")]);
    harness.update(cx, |state, cx| state.probe_helper(cx));
    assert_eq!(harness.fake.state().probes.len(), 1);
    resolve_probe(&harness, cx, 0, probe_of(true, false));
    assert!(matches!(
        harness.helper(cx),
        HelperUpgradeState::Confirming {
            operation: HelperOperation::Upgrade,
            ..
        }
    ));
}

#[gpui_kit::test]
fn a_coalesced_settings_check_does_not_keep_an_old_transport_probe_alive(cx: &mut TestAppContext) {
    let harness = ssh_host(cx, hold_probes);
    harness.publish(cx, 0, vec![state_frame("connected")]);
    harness.update(cx, |state, cx| state.probe_helper(cx));
    harness.publish(cx, 0, vec![state_frame("disconnected")]);
    assert_eq!(harness.helper(cx), HelperUpgradeState::Idle);
    resolve_probe(&harness, cx, 0, probe_of(true, false));
    assert_eq!(harness.helper(cx), HelperUpgradeState::Idle);
}

#[gpui_kit::test]
fn gives_a_settled_transport_its_own_probe(cx: &mut TestAppContext) {
    let harness = ssh_host(cx, hold_probes);
    harness.publish(
        cx,
        0,
        vec![error_frame(HANDSHAKE_FAILURE), state_frame("connected")],
    );
    assert_eq!(harness.fake.state().probes.len(), 2);
    resolve_probe(&harness, cx, 0, probe_of(true, false));
    assert!(matches!(
        harness.helper(cx),
        HelperUpgradeState::Probing { .. }
    ));
    resolve_probe(&harness, cx, 0, probe_of(true, true));
    assert!(matches!(
        harness.helper(cx),
        HelperUpgradeState::Ready { .. }
    ));
}

#[gpui_kit::test]
fn records_a_matching_probe_without_prompting(cx: &mut TestAppContext) {
    let harness = ssh_host(cx, |fake| queue_answer(fake, Ok(probe_of(true, true))));
    harness.publish(cx, 0, vec![state_frame("connected")]);
    assert_eq!(harness.fake.state().probes.len(), 1);
    assert!(matches!(
        harness.helper(cx),
        HelperUpgradeState::Ready { .. }
    ));
}

#[gpui_kit::test]
fn keeps_the_settings_check_informational(cx: &mut TestAppContext) {
    let harness = ssh_host(cx, |fake| queue_answer(fake, Ok(probe_of(true, false))));
    harness.update(cx, |state, cx| state.probe_helper(cx));
    assert_eq!(harness.fake.state().probes.len(), 1);
    assert!(matches!(
        harness.helper(cx),
        HelperUpgradeState::Ready { .. }
    ));
}

#[gpui_kit::test]
fn keeps_a_live_connection_when_its_secondary_probe_fails(cx: &mut TestAppContext) {
    let harness = ssh_host(cx, |fake| {
        queue_answer(fake, Err("Error: secondary SSH channel refused".into()));
    });
    harness.publish(cx, 0, vec![state_frame("connected")]);
    harness.read(cx, |state| {
        let link = state.active_link().unwrap();
        assert_eq!(link.host_state.phase, ConnectionPhase::Connected);
        assert_eq!(link.detail, "");
    });
    assert_eq!(
        harness.helper(cx),
        HelperUpgradeState::Failed {
            connection_key: "remote-a\0qa-host\0".into(),
            message: "Error: secondary SSH channel refused".into(),
            rollback: HelperRollback::NotNeeded,
            probe: None,
        }
    );
}

#[gpui_kit::test]
fn discards_a_probe_answer_after_the_app_switches_hosts(cx: &mut TestAppContext) {
    let harness = ssh_host(cx, hold_probes);
    harness.publish(cx, 0, vec![state_frame("connected")]);
    harness.update(cx, |state, cx| {
        state.connection = ConnectionSpec::Ssh {
            profile_id: "remote-b".into(),
            target: "other-host".into(),
            config_path: None,
        };
        state.settle(cx);
    });
    resolve_probe(&harness, cx, 0, probe_of(true, false));
    assert_eq!(harness.helper(cx), HelperUpgradeState::Idle);
}

#[gpui_kit::test]
fn abandons_a_pending_probe_when_an_explicit_reconnect_replaces_its_epoch(cx: &mut TestAppContext) {
    let harness = ssh_host(cx, hold_probes);
    harness.publish(cx, 0, vec![state_frame("connected")]);
    harness.update(cx, |state, cx| state.reconnect(cx));
    assert_eq!(harness.helper(cx), HelperUpgradeState::Idle);
    resolve_probe(&harness, cx, 0, probe_of(true, false));
    assert_eq!(harness.helper(cx), HelperUpgradeState::Idle);
}

#[gpui_kit::test]
fn reconciles_again_after_leaving_and_returning_to_the_same_profile(cx: &mut TestAppContext) {
    let harness = ssh_host(cx, |fake| {
        queue_answer(fake, Ok(probe_of(true, true)));
        queue_answer(fake, Ok(probe_of(true, true)));
    });
    harness.publish(cx, 0, vec![state_frame("connected")]);
    assert_eq!(harness.fake.state().probes.len(), 1);
    let remote = harness.read(cx, |state| state.connection().clone());
    harness.update(cx, |state, cx| {
        state.connection = ConnectionSpec::Local;
        state.settle(cx);
    });
    harness.update(cx, |state, cx| {
        state.connection = remote;
        state.settle(cx);
    });
    harness.publish(cx, 0, vec![state_frame("connected")]);
    assert_eq!(harness.fake.state().probes.len(), 2);
    assert!(matches!(
        harness.helper(cx),
        HelperUpgradeState::Ready { .. }
    ));
}

#[gpui_kit::test]
fn leaves_an_unreachable_host_with_the_error_it_actually_gave(cx: &mut TestAppContext) {
    let harness = ssh_host(cx, |fake| {
        queue_answer(fake, Err("no route to host".into()))
    });
    harness.publish(cx, 0, vec![error_frame(HANDSHAKE_FAILURE)]);
    assert_eq!(harness.fake.state().probes.len(), 1);
    assert!(matches!(
        harness.helper(cx),
        HelperUpgradeState::Failed { .. }
    ));
    harness.read(cx, |state| {
        assert!(
            state
                .active_link()
                .unwrap()
                .detail
                .contains("No such file or directory")
        );
    });
}

fn confirming(cx: &mut TestAppContext, prepare: impl FnOnce(&Fake)) -> Harness {
    let harness = ssh_host(cx, |fake| {
        queue_answer(fake, Ok(probe_of(false, false)));
        prepare(fake);
    });
    harness.publish(cx, 0, vec![error_frame(HANDSHAKE_FAILURE)]);
    assert!(matches!(
        harness.helper(cx),
        HelperUpgradeState::Confirming { .. }
    ));
    harness
}

#[gpui_kit::test]
fn a_consented_install_reconnects_for_a_fresh_snapshot(cx: &mut TestAppContext) {
    let harness = confirming(cx, |_| {});
    harness.update(cx, |state, cx| state.confirm_helper_install(cx));
    assert_eq!(harness.fake.state().installs.len(), 1);
    assert!(!harness.fake.state().installs[0].1);
    assert!(matches!(harness.helper(cx), HelperUpgradeState::Idle));
    assert_eq!(harness.fake.state().stopped, ["client-1"]);
    assert_eq!(harness.fake.bridge_count(), 2);
    harness.read(cx, |state| {
        assert_eq!(
            state.active_link().unwrap().detail,
            "Remote helper installed; reconnecting for a fresh authoritative snapshot."
        );
    });
}

#[gpui_kit::test]
fn a_refused_install_reports_and_stays_retryable(cx: &mut TestAppContext) {
    let harness = confirming(cx, |fake| {
        fake.state()
            .install_reports
            .push_back(Ok(HelperInstallReport {
                ok: false,
                message: "disk full".into(),
                rollback: HelperRollback::Restored,
            }));
    });
    let statuses = harness.statuses(cx);
    harness.update(cx, |state, cx| state.confirm_helper_install(cx));
    assert_eq!(
        statuses.lock().unwrap().as_slice(),
        ["Helper install failed: disk full"]
    );
    assert!(matches!(
        harness.helper(cx),
        HelperUpgradeState::Failed {
            rollback: HelperRollback::Restored,
            probe: Some(_),
            ..
        }
    ));
    assert_eq!(harness.fake.bridge_count(), 1);
}

#[gpui_kit::test]
fn an_install_that_fails_outright_reports_no_rollback(cx: &mut TestAppContext) {
    let harness = confirming(cx, |fake| {
        fake.state()
            .install_reports
            .push_back(Err("ssh dropped".into()));
    });
    harness.update(cx, |state, cx| state.confirm_helper_install(cx));
    assert!(matches!(
        harness.helper(cx),
        HelperUpgradeState::Failed {
            rollback: HelperRollback::NotNeeded,
            ..
        }
    ));
}

#[test]
fn pins_the_exact_shape_of_a_local_teardown_suffix() {
    assert_eq!(
        user_facing_bridge_failure("ssh closed (torn down locally: reader exited)"),
        "ssh closed"
    );
    assert_eq!(user_facing_bridge_failure("ssh closed"), "ssh closed");
}

#[gpui_kit::test]
fn ignores_an_error_that_is_not_a_handshake(cx: &mut TestAppContext) {
    let harness = ssh_host(cx, |_| {});
    harness.publish(cx, 0, vec![error_frame("qa-host: Connection refused")]);
    assert!(harness.fake.state().probes.is_empty());
    assert_eq!(harness.helper(cx), HelperUpgradeState::Idle);
}

#[gpui_kit::test]
fn ignores_a_handshake_failure_from_a_local_bridge(cx: &mut TestAppContext) {
    let harness = two_shown_hosts(cx);
    harness.publish(cx, 0, vec![error_frame(HANDSHAKE_FAILURE)]);
    assert!(harness.fake.state().probes.is_empty());
    assert_eq!(harness.helper(cx), HelperUpgradeState::Idle);
}

// Reconnect notices.

const UNREACHABLE: &str = "qa-host: Host is unreachable";
const REFUSED: &str = "qa-host: Connection refused";

fn count(statuses: &Mutex<Vec<String>>, message: &str) -> usize {
    statuses
        .lock()
        .unwrap()
        .iter()
        .filter(|item| *item == message)
        .count()
}

fn detail(harness: &Harness, cx: &mut TestAppContext) -> String {
    harness.read(cx, |state| state.active_link().unwrap().detail.clone())
}

fn phase(harness: &Harness, cx: &mut TestAppContext) -> ConnectionPhase {
    harness.read(cx, |state| state.active_host_state().unwrap().phase)
}

/// The bridge follows every error with the link's next state, as here. Tauri
/// drew only that state, so the error is never a notice of its own.
#[gpui_kit::test]
fn a_drop_the_next_state_supersedes_is_no_notice(cx: &mut TestAppContext) {
    let harness = ssh_host(cx, |fake| queue_answer(fake, Ok(probe_of(true, true))));
    let statuses = harness.statuses(cx);
    harness.publish(cx, 0, vec![state_frame("connected")]);
    harness.publish(cx, 0, vec![error_frame(UNREACHABLE)]);
    harness.publish(
        cx,
        0,
        vec![state_frame("disconnected"), state_frame("reconnecting")],
    );
    harness.publish(cx, 0, vec![state_frame("connected")]);
    assert_eq!(count(&statuses, UNREACHABLE), 0);
    assert_eq!(phase(&harness, cx), ConnectionPhase::Connected);
}

/// A link that stays down is reported where it was in Tauri: the phase the
/// strip and the host row draw, with the cause as the detail.
#[gpui_kit::test]
fn a_link_that_stays_down_keeps_its_cause_in_the_strip(cx: &mut TestAppContext) {
    let harness = ssh_host(cx, |fake| queue_answer(fake, Ok(probe_of(true, true))));
    let statuses = harness.statuses(cx);
    harness.publish(cx, 0, vec![state_frame("connected")]);
    harness.publish(
        cx,
        0,
        vec![error_frame(UNREACHABLE), state_frame("disconnected")],
    );
    assert_eq!(phase(&harness, cx), ConnectionPhase::Disconnected);
    assert_eq!(detail(&harness, cx), UNREACHABLE);

    harness.publish(
        cx,
        0,
        vec![error_frame(REFUSED), state_frame("disconnected")],
    );
    assert_eq!(detail(&harness, cx), REFUSED);
    assert_eq!(count(&statuses, UNREACHABLE) + count(&statuses, REFUSED), 0);
}

/// Nothing follows a detach, so it stays a notice.
#[gpui_kit::test]
fn a_detach_is_still_a_notice(cx: &mut TestAppContext) {
    let harness = ssh_host(cx, |fake| queue_answer(fake, Ok(probe_of(true, true))));
    let statuses = harness.statuses(cx);
    harness.publish(cx, 0, vec![state_frame("connected")]);
    harness.publish(cx, 0, vec![frame(5, "session ended", 1, &[])]);
    assert_eq!(count(&statuses, "Detached: session ended"), 1);
}

#[gpui_kit::test]
fn strips_the_local_teardown_suffix_from_what_the_user_reads(cx: &mut TestAppContext) {
    let harness = ssh_host(cx, |fake| queue_answer(fake, Ok(probe_of(true, true))));
    harness.publish(cx, 0, vec![state_frame("connected")]);
    harness.publish(
        cx,
        0,
        vec![error_frame(
            "connection lost (torn down locally: reader task exited)",
        )],
    );
    assert_eq!(detail(&harness, cx), "connection lost");
}

const VERDICT: &str =
    "Your connection to qa-host is unstable; Muxflow keeps losing the link and reconnecting.";

#[gpui_kit::test]
fn the_first_tmux_server_starting_is_no_lost_link(cx: &mut TestAppContext) {
    const CHANGED: &str = "tmux server identity changed; authoritative reconnect required";
    let harness = ssh_host(cx, |fake| queue_answer(fake, Ok(probe_of(true, true))));
    let statuses = harness.statuses(cx);
    harness.publish(
        cx,
        0,
        vec![state_frame("connected"), world("tmux:none", 1, &[])],
    );
    harness.publish(cx, 0, vec![error_frame(CHANGED)]);
    harness.publish(
        cx,
        0,
        vec![
            state_frame("connected"),
            world("srv-qa", 2, &[("$0", "home")]),
        ],
    );
    // One real loss after it: not yet a link that keeps dropping.
    harness.publish(cx, 0, vec![error_frame(UNREACHABLE)]);
    assert_eq!(count(&statuses, VERDICT), 0);
}

/// Only that exact case is excused: a running server replaced, or any other
/// drop on a host with no server, still counts toward the verdict.
#[gpui_kit::test]
fn every_other_drop_counts_as_a_lost_link(cx: &mut TestAppContext) {
    const CHANGED: &str = "tmux server identity changed; authoritative reconnect required";
    for (identity, first) in [("srv-qa", CHANGED), ("tmux:none", UNREACHABLE)] {
        let harness = ssh_host(cx, |fake| {
            queue_answer(fake, Ok(probe_of(true, true)));
            queue_answer(fake, Ok(probe_of(true, true)));
        });
        let statuses = harness.statuses(cx);
        harness.publish(
            cx,
            0,
            vec![state_frame("connected"), world(identity, 1, &[])],
        );
        harness.publish(cx, 0, vec![error_frame(first)]);
        harness.publish(cx, 0, vec![state_frame("connected")]);
        harness.publish(cx, 0, vec![error_frame(UNREACHABLE)]);
        assert_eq!(count(&statuses, VERDICT), 1, "{identity}");
    }
}

#[gpui_kit::test]
fn a_second_drop_inside_the_window_speaks_the_verdict(cx: &mut TestAppContext) {
    let harness = ssh_host(cx, |fake| {
        queue_answer(fake, Ok(probe_of(true, true)));
        queue_answer(fake, Ok(probe_of(true, true)));
    });
    let statuses = harness.statuses(cx);
    harness.publish(cx, 0, vec![state_frame("connected")]);
    harness.publish(cx, 0, vec![error_frame(UNREACHABLE)]);
    harness.publish(
        cx,
        0,
        vec![state_frame("reconnecting"), state_frame("connected")],
    );
    harness.publish(cx, 0, vec![error_frame(UNREACHABLE)]);
    assert_eq!(count(&statuses, VERDICT), 1);
    assert_eq!(count(&statuses, UNREACHABLE), 0);
    assert_eq!(detail(&harness, cx), VERDICT);
}

// Several hosts.

#[gpui_kit::test]
fn starts_every_shown_host_attaching_a_terminal_only_on_the_active_one(cx: &mut TestAppContext) {
    let harness = two_shown_hosts(cx);
    let attached: Vec<(String, bool)> = harness
        .fake
        .state()
        .bridges
        .iter()
        .map(|bridge| (host_profile_id(&bridge.connection), bridge.attach))
        .collect();
    assert_eq!(
        attached,
        [("local".to_owned(), true), ("remote-a".to_owned(), false)]
    );
    harness.read(cx, |state| {
        assert_eq!(state.active_profile_id(), "local");
        assert_eq!(
            state.link("local").unwrap().client_id.as_deref(),
            Some("client-1")
        );
        assert_eq!(
            state.link("remote-a").unwrap().client_id.as_deref(),
            Some("client-2")
        );
    });
}

#[gpui_kit::test]
fn activates_a_peer_at_its_remembered_session_without_restarting_it(cx: &mut TestAppContext) {
    let harness = two_shown_hosts(cx);
    harness.publish(
        cx,
        0,
        vec![
            state_frame("connected"),
            world("srv-local", 1, &[("$0", "home")]),
        ],
    );
    harness.publish(
        cx,
        1,
        vec![
            state_frame("connected"),
            world("srv-qa", 1, &[("$1", "build"), ("$2", "work")]),
        ],
    );
    harness.read(cx, |state| {
        assert_eq!(
            state.link("remote-a").unwrap().active_session_id.as_deref(),
            Some("$1")
        );
        assert_eq!(
            state.active_link().unwrap().active_session_id.as_deref(),
            Some("$0")
        );
    });

    harness.update(cx, |state, cx| state.activate_host("remote-a", cx));
    assert_eq!(harness.fake.bridge_count(), 2);
    assert!(harness.fake.state().stopped.is_empty());
    harness.read(cx, |state| {
        assert_eq!(state.active_profile_id(), "remote-a");
        assert_eq!(state.selected_profile_id(), "remote-a");
        assert_eq!(state.ssh_target(), "qa-host");
        assert_eq!(state.connection_mode(), ConnectionMode::Ssh);
        let link = state.active_link().unwrap();
        assert_eq!(link.client_id.as_deref(), Some("client-2"));
        assert_eq!(link.active_session_id.as_deref(), Some("$1"));
        assert_eq!(
            state
                .active_host_state()
                .unwrap()
                .server_identity
                .as_deref(),
            Some("srv-qa")
        );
    });
}

#[gpui_kit::test]
fn stops_a_hosts_bridge_when_it_stops_being_shown(cx: &mut TestAppContext) {
    let harness = two_shown_hosts(cx);
    harness.update(cx, |state, cx| {
        state.save_profile_fields("remote-a", None, false, cx)
    });
    assert_eq!(harness.fake.state().stopped, ["client-2"]);
    assert_eq!(harness.fake.bridge_count(), 2);
    harness.read(cx, |state| {
        assert_eq!(state.links().count(), 1);
        assert!(state.link("remote-a").is_none());
        assert_eq!(
            state.active_link().unwrap().client_id.as_deref(),
            Some("client-1")
        );
    });
}

#[gpui_kit::test]
fn keeps_the_active_host_untouched_by_a_peers_events(cx: &mut TestAppContext) {
    let harness = two_shown_hosts(cx);
    harness.publish(
        cx,
        0,
        vec![
            state_frame("connected"),
            world("srv-local", 1, &[("$0", "home")]),
        ],
    );
    let before = harness.read(cx, |state| state.active_link().unwrap().clone());
    harness.publish(
        cx,
        1,
        vec![
            state_frame("connected"),
            world("srv-qa", 1, &[("$1", "work")]),
            error_frame("qa-host: Connection refused"),
        ],
    );
    harness.read(cx, |state| {
        assert_eq!(state.active_link().unwrap(), &before);
        let peer = state.link("remote-a").unwrap();
        assert_eq!(peer.active_session_id.as_deref(), Some("$1"));
        assert_eq!(peer.detail, "qa-host: Connection refused");
        assert_eq!(peer.host_state.session("$1").unwrap().name, "work");
    });
}

#[gpui_kit::test]
fn keeps_a_peers_failures_and_helper_questions_off_the_screen(cx: &mut TestAppContext) {
    let harness = two_shown_hosts(cx);
    let statuses = harness.statuses(cx);
    harness.publish(
        cx,
        1,
        vec![state_frame("connected"), error_frame(HANDSHAKE_FAILURE)],
    );
    assert!(statuses.lock().unwrap().is_empty());
    assert!(harness.fake.state().probes.is_empty());
    assert_eq!(harness.helper(cx), HelperUpgradeState::Idle);
}

#[gpui_kit::test]
fn restarts_only_the_stalled_peers_bridge_and_only_once(cx: &mut TestAppContext) {
    let harness = two_shown_hosts(cx);
    let local_epoch = harness.read(cx, |state| state.link("local").unwrap().connection_epoch);
    harness.publish(cx, 1, vec![epoch_frame(9)]);
    harness.publish(cx, 1, vec![stall_frame("%0", 1), stall_frame("%1", 2)]);
    assert_eq!(harness.fake.state().stopped, ["client-2"]);
    assert_eq!(harness.fake.bridge_count(), 3);
    let state = harness.fake.state();
    assert_eq!(state.bridges[2].client_id, "client-3");
    assert!(!state.bridges[2].attach);
    drop(state);
    harness.read(cx, |state| {
        assert_eq!(state.link("local").unwrap().connection_epoch, local_epoch);
        assert_eq!(
            state.link("remote-a").unwrap().detail,
            "A terminal output stream stalled; reconnecting it now."
        );
        assert_eq!(state.active_link().unwrap().detail, "");
    });
}

#[gpui_kit::test]
fn a_stall_outside_the_live_epoch_restarts_nothing(cx: &mut TestAppContext) {
    let harness = two_shown_hosts(cx);
    harness.publish(cx, 1, vec![stall_frame("%0", 1)]);
    assert!(harness.fake.state().stopped.is_empty());
    assert_eq!(harness.fake.bridge_count(), 2);
}

// Resume from sleep.

#[gpui_kit::test]
fn rebuilds_every_link_when_the_active_link_does_not_answer_and_none_when_it_does(
    cx: &mut TestAppContext,
) {
    let harness = two_shown_hosts(cx);
    harness.publish(
        cx,
        0,
        vec![
            state_frame("connected"),
            world("srv-local", 1, &[("$0", "home")]),
        ],
    );
    let epochs = |harness: &Harness, cx: &mut TestAppContext| {
        harness.read(cx, |state| {
            state
                .links()
                .map(|link| link.connection_epoch)
                .collect::<Vec<_>>()
        })
    };
    let before = epochs(&harness, cx);

    harness.update(cx, |state, cx| state.desktop_resumed(cx));
    assert!(harness.fake.state().stopped.is_empty());
    assert_eq!(harness.fake.state().discards, 0);
    assert_eq!(epochs(&harness, cx), before);
    assert!(
        harness
            .fake
            .incident_kinds()
            .contains(&"resume.linkAlive".to_owned())
    );

    harness.fake.state().now += 2_000;
    harness
        .fake
        .state()
        .link_probes
        .push_back(Err("timed out".into()));
    harness.update(cx, |state, cx| state.desktop_resumed(cx));
    assert_eq!(harness.fake.state().discards, 1);
    let mut stopped = harness.fake.state().stopped.clone();
    stopped.sort();
    assert_eq!(stopped, ["client-1", "client-2"]);
    assert_eq!(harness.fake.bridge_count(), 4);
    let after = epochs(&harness, cx);
    assert!(after.iter().zip(&before).all(|(new, old)| new > old));
    harness.read(cx, |state| {
        for link in state.links() {
            assert_eq!(
                link.detail,
                "System resumed; reconnecting for an authoritative state refresh."
            );
        }
    });
    let state = harness.fake.state();
    let last = |id: &str| {
        state
            .bridges
            .iter()
            .rev()
            .find(|bridge| host_profile_id(&bridge.connection) == id)
            .unwrap()
    };
    assert!(last("local").attach);
    assert!(!last("remote-a").attach);
}

#[gpui_kit::test]
fn a_link_that_never_answers_is_rebuilt_at_the_timeout(cx: &mut TestAppContext) {
    let harness = two_shown_hosts(cx);
    harness.publish(cx, 0, vec![state_frame("connected")]);
    harness.fake.state().hold_link_probes = true;
    harness.update(cx, |state, cx| state.desktop_resumed(cx));
    assert_eq!(harness.fake.state().discards, 0);
    cx.executor()
        .advance_clock(Duration::from_millis(RESUME_PROBE_TIMEOUT_MS));
    cx.run_until_parked();
    assert_eq!(harness.fake.state().discards, 1);
    assert_eq!(harness.fake.bridge_count(), 4);
}

#[gpui_kit::test]
fn an_unconnected_link_has_nothing_to_probe_and_rebuilds(cx: &mut TestAppContext) {
    let harness = two_shown_hosts(cx);
    harness.update(cx, |state, cx| state.desktop_resumed(cx));
    assert_eq!(harness.fake.state().discards, 1);
    let probe = harness
        .fake
        .state()
        .incidents
        .iter()
        .find(|(kind, _)| kind == "reconnect.desktopResume")
        .map(|(_, detail)| detail.clone())
        .unwrap();
    assert_eq!(probe, json!({ "trigger": "native", "probe": "skipped" }));
}

#[gpui_kit::test]
fn a_probe_outcome_for_a_replaced_bridge_is_not_a_rebuild(cx: &mut TestAppContext) {
    let harness = two_shown_hosts(cx);
    harness.publish(cx, 0, vec![state_frame("connected")]);
    harness.fake.state().hold_link_probes = true;
    harness.update(cx, |state, cx| state.desktop_resumed(cx));
    harness.update(cx, |state, cx| state.reconnect(cx));
    let bridges = harness.fake.bridge_count();
    cx.executor()
        .advance_clock(Duration::from_millis(RESUME_PROBE_TIMEOUT_MS));
    cx.run_until_parked();
    assert_eq!(harness.fake.state().discards, 0);
    assert_eq!(harness.fake.bridge_count(), bridges);
}

#[gpui_kit::test]
fn a_timer_gap_in_the_foreground_reconnects_and_a_background_gap_does_not(cx: &mut TestAppContext) {
    let harness = two_shown_hosts(cx);
    harness.fake.state().now += 60_000;
    harness.update(cx, |state, cx| state.tick(false, cx));
    assert_eq!(harness.fake.state().discards, 0);

    harness.fake.state().now += 60_000;
    harness.update(cx, |state, cx| state.tick(true, cx));
    assert_eq!(
        harness.fake.state().discards,
        0,
        "returning to front resets the timer"
    );
    harness.fake.state().now += 60_000;
    harness.update(cx, |state, cx| state.tick(true, cx));
    assert_eq!(harness.fake.state().discards, 1);
}

#[gpui_kit::test]
fn an_offline_to_online_transition_reconnects(cx: &mut TestAppContext) {
    let harness = two_shown_hosts(cx);
    harness.fake.state().online = false;
    harness.update(cx, |state, cx| state.tick(true, cx));
    assert_eq!(harness.fake.state().discards, 0);
    harness.fake.state().online = true;
    harness.update(cx, |state, cx| state.tick(true, cx));
    assert_eq!(harness.fake.state().discards, 1);
}

// Bridge lifecycle.

#[gpui_kit::test]
fn credit_waits_for_the_engine_to_consume_the_frame(cx: &mut TestAppContext) {
    let harness = two_shown_hosts(cx);
    harness.publish(cx, 0, vec![epoch_frame(7)]);
    let before = harness.fake.state().acks.len();
    harness.fake.publish(0, state_frame("connected"));
    cx.run_until_parked();
    assert_eq!(harness.fake.state().acks.len(), before);
    cx.executor().advance_clock(POLL_IDLE);
    cx.run_until_parked();
    assert_eq!(harness.fake.state().acks.len(), before + 1);
}

#[gpui_kit::test]
fn acknowledges_every_admitted_frame_including_undecodable_ones(cx: &mut TestAppContext) {
    let harness = two_shown_hosts(cx);
    let undecodable = vec![42, 0, 0];
    let epoch = epoch_frame(7);
    let state = state_frame("connected");
    let total = (undecodable.len() + epoch.len() + state.len()) as u64;
    harness.publish(cx, 0, vec![epoch, undecodable, state]);
    let acks = harness.fake.state().acks.clone();
    assert_eq!(acks.last().unwrap(), &("client-1".to_owned(), 7, 3, total));
    assert!(
        harness
            .fake
            .incident_kinds()
            .contains(&"link.decodeFailure".to_owned())
    );
    harness.read(cx, |state| {
        assert_eq!(state.active_link().unwrap().terminal_epoch, 7);
    });
}

#[gpui_kit::test]
fn a_failed_start_reports_its_error_without_a_client(cx: &mut TestAppContext) {
    let harness = harness_with(
        cx,
        vec![local_profile(true)],
        "local",
        |fake| fake.state().start_failure = Some("host helper is missing".into()),
        |_| {},
    );
    harness.read(cx, |state| {
        let link = state.active_link().unwrap();
        assert_eq!(link.detail, "host helper is missing");
        assert_eq!(link.client_id, None);
        assert_eq!(state.status(), "host helper is missing");
    });
}

#[gpui_kit::test]
fn the_journal_records_what_the_user_saw(cx: &mut TestAppContext) {
    let harness = two_shown_hosts(cx);
    harness.publish(cx, 0, vec![state_frame("connected")]);
    harness.publish(cx, 0, vec![frame(6, "resyncing", 0, b"sequence gap")]);
    harness.publish(cx, 0, vec![state_frame("connected")]);
    let kinds = harness.fake.incident_kinds();
    for expected in [
        "link.resyncStarted",
        "link.degraded",
        "link.resynced",
        "link.restored",
    ] {
        assert!(kinds.contains(&expected.to_owned()), "{expected}");
    }
}

/// Port of `postResyncReseed.test.tsx`: a repaired stream reseeds the panes
/// on screen; an ordinary connect or reconnect does not.
#[gpui_kit::test]
fn only_a_resync_repair_reseeds_the_mounted_panes(cx: &mut TestAppContext) {
    let harness = two_shown_hosts(cx);
    harness.publish(cx, 0, vec![epoch_frame(7), state_frame("connected")]);
    let link = harness.read(cx, |state| state.terminal_link("local").unwrap());
    cx.update(|cx| {
        link.read(cx).send(terminal_engine::Command::Mount {
            pane_id: "%1".into(),
            size: terminal_engine::GridSize {
                columns: 80,
                rows: 24,
            },
        })
    });
    harness.publish(cx, 0, vec![]);
    let seeds = || harness.fake.state().seeds.len();
    let mounted = seeds();
    harness.publish(cx, 0, vec![state_frame("reconnecting")]);
    harness.publish(cx, 0, vec![state_frame("connected")]);
    harness.publish(cx, 0, vec![state_frame("connecting")]);
    harness.publish(cx, 0, vec![state_frame("connected")]);
    assert_eq!(seeds(), mounted, "a plain reconnect reseeded");
    harness.publish(cx, 0, vec![frame(6, "resyncing", 0, b"sequence gap")]);
    assert_eq!(seeds(), mounted, "reseeded before the repair finished");
    harness.publish(cx, 0, vec![state_frame("connected")]);
    assert_eq!(seeds(), mounted + 1);
    assert_eq!(harness.fake.state().seeds.last().unwrap(), "%1");
}

/// The file-transfer connection is warmed for the host on screen, once per
/// writable epoch, and never for a peer beside it.
#[gpui_kit::test]
fn warms_file_transfer_for_the_active_host_only(cx: &mut TestAppContext) {
    let harness = two_shown_hosts(cx);
    let prewarms = || harness.fake.state().prewarms.clone();
    harness.publish(cx, 0, vec![epoch_frame(7)]);
    assert!(prewarms().is_empty(), "warmed before the host was writable");
    harness.publish(cx, 0, vec![state_frame("connected")]);
    assert_eq!(prewarms(), ["client-1"]);
    harness.publish(cx, 0, vec![state_frame("connected")]);
    let peer = harness.fake.bridge_index("client-2");
    harness.publish(cx, peer, vec![epoch_frame(3), state_frame("connected")]);
    assert_eq!(prewarms(), ["client-1"], "a peer or a repeat warmed");
    harness.publish(cx, 0, vec![state_frame("reconnecting")]);
    harness.publish(cx, 0, vec![epoch_frame(8), state_frame("connected")]);
    assert_eq!(
        prewarms(),
        ["client-1", "client-1"],
        "a new epoch earns one warmup"
    );
}

#[gpui_kit::test]
fn the_journal_sees_a_repair_that_lands_in_one_batch(cx: &mut TestAppContext) {
    let harness = two_shown_hosts(cx);
    harness.publish(cx, 0, vec![state_frame("connected")]);
    harness.publish(
        cx,
        0,
        vec![
            frame(6, "resyncing", 0, b"sequence gap"),
            state_frame("connected"),
        ],
    );
    let kinds = harness.fake.incident_kinds();
    for expected in ["link.degraded", "link.resynced", "link.restored"] {
        assert!(kinds.contains(&expected.to_owned()), "{expected}");
    }
}

#[gpui_kit::test]
fn terminal_output_is_acknowledged_without_touching_connection_state(cx: &mut TestAppContext) {
    let harness = two_shown_hosts(cx);
    harness.publish(cx, 0, vec![epoch_frame(7), state_frame("connected")]);
    let notifications = Arc::new(Mutex::new(0));
    let counter = notifications.clone();
    let controller = harness.controller.clone();
    cx.update(|cx| {
        cx.observe(&controller, move |_, _| *counter.lock().unwrap() += 1)
            .detach();
    });
    let acks_before = harness.fake.state().acks.len();
    harness.publish(cx, 0, vec![frame(2, "%1", 4, b"output")]);
    assert_eq!(*notifications.lock().unwrap(), 0);
    assert_eq!(harness.fake.state().acks.len(), acks_before + 1);
}

#[gpui_kit::test]
fn a_late_request_is_slow_network_once_the_counter_moves(cx: &mut TestAppContext) {
    let harness = ssh_host(cx, |fake| queue_answer(fake, Ok(probe_of(true, true))));
    let statuses = harness.statuses(cx);
    harness.publish(cx, 0, vec![state_frame("connected")]);
    harness.fake.state().late_requests = Some(3);
    harness.update(cx, |state, cx| state.tick(true, cx));
    assert!(
        statuses
            .lock()
            .unwrap()
            .iter()
            .all(|status| !status.contains("slow"))
    );
    harness.fake.state().late_requests = Some(4);
    harness.update(cx, |state, cx| state.tick(true, cx));
    assert_eq!(
        count(
            &statuses,
            "Your connection to qa-host is slow; the host is answering, but late."
        ),
        1
    );
}

// App tabs left by a replaced server.

fn tab(server: &str, session_id: &str) -> AppTabRecord {
    AppTabRecord {
        id: "tab-a".into(),
        host_profile_id: "remote-a".into(),
        server_identity: server.into(),
        session_id: session_id.into(),
        session_name: "workspace".into(),
        kind: AppTabKind::File,
        resource: "/repo/a.ts".into(),
        root_path: None,
        root_token: None,
        preview: None,
        view_mode: None,
        git_repository_id: None,
        git_path: None,
        git_original_path: None,
        git_target: None,
        git_status_generation: None,
        git_source_generation: None,
        title: "a.ts".into(),
        order: 0,
    }
}

fn with_app_tab(cx: &mut TestAppContext) -> Harness {
    let harness = harness_with(
        cx,
        vec![ssh_profile("remote-a", "qa-host", true)],
        "remote-a",
        |_| {},
        |dir| {
            let state = json!({
                "schemaVersion": 1,
                "appTabs": [tab("server-a", "$1")],
                "workspaceUi": [],
                "shell": { "theme": "system", "uiScale": 1.0 },
            });
            std::fs::write(dir.join("app-state.json"), state.to_string()).unwrap();
        },
    );
    harness.fake.state().hold_probes = true;
    harness
}

fn stored_tabs(harness: &Harness) -> Vec<AppTabRecord> {
    let state = harness.fake.state();
    state
        .saved_app_states
        .last()
        .expect("a saved state")
        .app_tabs
        .clone()
}

#[gpui_kit::test]
fn offers_tabs_from_a_replaced_server_across_the_reconnect_gap(cx: &mut TestAppContext) {
    let harness = with_app_tab(cx);
    harness.publish(cx, 0, vec![world("server-a", 1, &[("$1", "workspace")])]);
    harness.read(cx, |state| assert!(state.app_recovery().is_none()));
    harness.update(cx, |state, cx| state.reconnect(cx));
    harness.read(cx, |state| assert!(state.app_recovery().is_none()));
    harness.publish(cx, 1, vec![world("server-b", 1, &[("$99", "workspace")])]);
    harness.read(cx, |state| {
        let offer = state.app_recovery().unwrap();
        assert_eq!(offer.count, 1);
        assert_eq!(offer.previous_server_identity, "server-a");
        assert_eq!(offer.scope.server_identity.as_deref(), Some("server-b"));
    });

    harness.update(cx, |state, cx| state.restore_app_tabs(cx));
    harness.read(cx, |state| assert!(state.app_recovery().is_none()));
    let tabs = stored_tabs(&harness);
    assert_eq!(tabs[0].server_identity, "server-b");
    assert_eq!(tabs[0].session_id, "$99");
}

#[gpui_kit::test]
fn discarding_asks_first_and_removes_only_the_replaced_servers_tabs(cx: &mut TestAppContext) {
    let harness = with_app_tab(cx);
    harness.publish(cx, 0, vec![world("server-a", 1, &[("$1", "workspace")])]);
    harness.publish(cx, 0, vec![world("server-b", 1, &[("$99", "workspace")])]);
    harness.update(cx, |state, cx| state.request_app_tab_discard(cx));
    harness.read(cx, |state| assert!(state.app_recovery_modal_open()));
    harness.update(cx, |state, cx| state.cancel_app_tab_discard(cx));
    harness.read(cx, |state| {
        assert!(!state.app_recovery_modal_open());
        assert!(state.app_recovery().is_some());
    });
    harness.update(cx, |state, cx| state.request_app_tab_discard(cx));
    harness.update(cx, |state, cx| state.confirm_app_tab_discard(cx));
    harness.read(cx, |state| assert!(state.app_recovery().is_none()));
    assert!(stored_tabs(&harness).is_empty());
}

#[gpui_kit::test]
fn a_failed_save_says_so_and_keeps_the_change(cx: &mut TestAppContext) {
    let harness = with_app_tab(cx);
    harness.publish(cx, 0, vec![world("server-a", 1, &[("$1", "workspace")])]);
    harness.publish(cx, 0, vec![world("server-b", 1, &[("$99", "workspace")])]);
    harness.fake.state().save_failure = Some("disk full".into());
    let statuses = harness.statuses(cx);
    harness.update(cx, |state, cx| state.restore_app_tabs(cx));
    assert_eq!(
        statuses.lock().unwrap().as_slice(),
        ["Could not save application tabs; changes will be retried: disk full"]
    );
    harness.read(cx, |state| {
        let kept = state.unsaved_app_state.as_ref().unwrap();
        assert_eq!(kept.app_tabs[0].server_identity, "server-b");
    });
}

#[gpui_kit::test]
fn a_discard_is_only_confirmed_from_its_confirmation(cx: &mut TestAppContext) {
    let harness = with_app_tab(cx);
    harness.publish(cx, 0, vec![world("server-a", 1, &[("$1", "workspace")])]);
    harness.publish(cx, 0, vec![world("server-b", 1, &[("$99", "workspace")])]);
    let saves = harness.fake.state().saved_app_states.len();
    harness.update(cx, |state, cx| state.confirm_app_tab_discard(cx));
    assert_eq!(harness.fake.state().saved_app_states.len(), saves);
    harness.read(cx, |state| assert!(state.app_recovery().is_some()));
}

#[gpui_kit::test]
fn an_offer_goes_with_the_connection_it_was_made_for(cx: &mut TestAppContext) {
    let harness = with_app_tab(cx);
    harness.publish(cx, 0, vec![world("server-a", 1, &[("$1", "workspace")])]);
    harness.publish(cx, 0, vec![world("server-b", 1, &[("$99", "workspace")])]);
    harness.read(cx, |state| assert!(state.app_recovery().is_some()));
    harness.update(cx, |state, cx| state.reconnect(cx));
    harness.read(cx, |state| assert!(state.app_recovery().is_none()));
}

#[gpui_kit::test]
fn a_reset_of_recovered_profiles_lands_on_local_with_a_fresh_bridge(cx: &mut TestAppContext) {
    let harness = ssh_host(cx, |_| {});
    let statuses = harness.statuses(cx);
    harness.update(cx, |state, cx| state.confirm_profile_reset(cx));
    harness.read(cx, |state| {
        assert_eq!(state.connection(), &ConnectionSpec::Local);
        assert_eq!(state.profiles().len(), 1);
        assert_eq!(state.connection_mode(), ConnectionMode::Local);
    });
    assert_eq!(
        statuses.lock().unwrap().as_slice(),
        ["Recovered host profiles were reset to Local; the invalid original remains preserved."]
    );
    let local = harness.fake.bridge_index("client-2");
    assert_eq!(
        harness.fake.state().bridges[local].connection,
        ConnectionSpec::Local
    );
}

#[gpui_kit::test]
fn an_unreadable_state_keeps_tabs_in_memory_until_a_reset_preserves_it(cx: &mut TestAppContext) {
    let harness = harness_with(
        cx,
        vec![local_profile(true)],
        "local",
        |_| {},
        |dir| std::fs::write(dir.join("app-state.json"), "{ not json").unwrap(),
    );
    let recovery = harness.read(cx, |state| state.app_state_recovery().map(str::to_owned));
    assert!(
        recovery.as_deref().is_some_and(
            |error| error.starts_with("Saved shell state requires recovery: invalid app state")
        ),
        "{recovery:?}"
    );
    // Frozen: a change holds in memory and nothing is written.
    harness.update(cx, |state, cx| {
        state.update_app_state(
            |state| {
                state.workspace_ui.clear();
                true
            },
            cx,
        )
    });
    assert!(harness.fake.state().saved_app_states.is_empty());
    harness.read(cx, |state| assert!(state.unsaved_app_state.is_some()));

    // Preferences and shortcuts changed during the frozen run are live.
    cx.update(|cx| {
        gpui_kit::init(cx);
        crate::app::init(
            crate::app::Core::at(Path::new("/nonexistent/app-state.json")),
            cx,
        );
        let mut changed = PersistedAppState::default();
        changed.shell.copy_on_select = Some(!crate::settings::get(cx).copy_on_select);
        changed.commands.shortcut_overrides =
            [("commands.show".to_owned(), Some("Ctrl+Shift+J".to_owned()))].into();
        crate::settings::reload(&changed, cx);
        crate::commands::reload_overrides(&changed, cx);
    });

    let statuses = harness.statuses(cx);
    harness.update(cx, |state, cx| state.confirm_app_state_reset(cx));
    assert_eq!(
        statuses.lock().unwrap().as_slice(),
        ["Saved shell state was reset; the invalid original was preserved for recovery."]
    );
    harness.read(cx, |state| {
        assert_eq!(state.app_state_recovery(), None);
        assert!(state.unsaved_app_state.is_none());
    });
    cx.read(|cx| {
        let defaults = PersistedAppState::default();
        assert_eq!(
            crate::settings::get(cx),
            &crate::settings::Prefs::from_state(&defaults),
            "the reset's defaults are the live preferences"
        );
        assert!(
            cx.global::<crate::commands::Commands>()
                .overrides()
                .is_empty()
        );
    });
    let dir = harness._dir.path();
    let preserved = std::fs::read_dir(dir)
        .unwrap()
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.file_name().to_string_lossy().contains("invalid-"))
        .count();
    assert_eq!(
        preserved, 1,
        "the invalid original is kept beside the new file"
    );
    assert!(load_app_state(&AppStateStore::load(dir.join("app-state.json"))).is_ok());
}

// The shell surface.

mod surface {
    use std::path::Path;

    use gpui_kit::{
        KeyDownEvent, KeyUpEvent, Keystroke, Role, TestAppContext, VisualTestContext,
        test::{TestQueryExt, TestWindowExt},
    };

    use super::*;
    use crate::{
        app::{self, Core},
        connection_ui::{ConnectionUi, surface::STRIP_APPEAR_DELAY_MS},
    };

    fn open(cx: &mut TestAppContext, harness: &Harness) -> VisualTestContext {
        let controller = harness.controller.clone();
        let window = cx.update(|cx| {
            gpui_kit::init(cx);
            app::init(Core::at(Path::new("/nonexistent/app-state.json")), cx);
            cx.set_global(ConnectionUi(controller));
            app::open_main_window(cx).unwrap()
        });
        let visual = VisualTestContext::from_window(window, cx);
        visual.run_until_parked();
        visual
    }

    fn role_of(visual: &mut VisualTestContext, label: &str) -> Option<Role> {
        visual.update(|window, _| window.find_by_label(label).role())
    }

    #[gpui_kit::test]
    fn the_host_row_is_a_named_button_that_follows_the_phase(cx: &mut TestAppContext) {
        let harness = two_shown_hosts(cx);
        let mut visual = open(cx, &harness);
        assert_eq!(
            role_of(
                &mut visual,
                "Host Local over local, disconnected. Open the host menu."
            ),
            Some(Role::Button)
        );
        harness.fake.publish(0, state_frame("connected"));
        visual.run_until_parked();
        assert_eq!(
            role_of(
                &mut visual,
                "Host Local over local, connected. Open the host menu."
            ),
            Some(Role::Button)
        );
    }

    #[gpui_kit::test]
    fn the_strip_waits_then_names_its_status_and_actions(cx: &mut TestAppContext) {
        let harness = two_shown_hosts(cx);
        let mut visual = open(cx, &harness);
        harness.fake.publish(0, state_frame("disconnected"));
        visual.run_until_parked();
        let before = visual.update(|window, _| window.try_find_by_label("Reconnect").is_none());
        assert!(before, "a brief dip stays silent");

        visual
            .executor()
            .advance_clock(Duration::from_millis(STRIP_APPEAR_DELAY_MS));
        visual.run_until_parked();
        assert_eq!(role_of(&mut visual, "Reconnect"), Some(Role::Button));
        assert_eq!(role_of(&mut visual, "Connection…"), Some(Role::Button));

        harness.fake.publish(0, state_frame("connected"));
        visual.run_until_parked();
        let gone = visual.update(|window, _| window.try_find_by_label("Reconnect").is_none());
        assert!(gone, "recovery is not delayed");
    }

    #[gpui_kit::test]
    fn the_helper_consent_dialog_stays_open_and_turns_into_progress(cx: &mut TestAppContext) {
        let harness = confirming(cx, |fake| fake.state().hold_installs = true);
        let mut visual = open(cx, &harness);
        visual.run_until_parked();
        assert_eq!(
            role_of(&mut visual, "Install and connect"),
            Some(Role::Button)
        );
        harness
            .controller
            .update(&mut visual, |state, cx| state.confirm_helper_install(cx));
        visual.run_until_parked();
        assert!(matches!(
            harness.helper(&mut visual),
            HelperUpgradeState::Upgrading { .. }
        ));
        assert_eq!(role_of(&mut visual, "Working…"), Some(Role::Button));
        // Tauri's `SetupProgress`: the busy detail is spoken as a status.
        assert_eq!(
            role_of(
                &mut visual,
                "Uploading and verifying the helper. This may take a little while."
            ),
            Some(Role::Status)
        );
    }

    #[gpui_kit::test]
    fn the_discard_confirmation_goes_with_its_offer(cx: &mut TestAppContext) {
        let harness = with_app_tab(cx);
        harness.publish(cx, 0, vec![world("server-a", 1, &[("$1", "workspace")])]);
        harness.publish(cx, 0, vec![world("server-b", 1, &[("$99", "workspace")])]);
        let mut visual = open(cx, &harness);
        harness
            .controller
            .update(&mut visual, |state, cx| state.request_app_tab_discard(cx));
        visual.run_until_parked();
        assert_eq!(role_of(&mut visual, "Discard old tabs"), Some(Role::Button));
        harness
            .controller
            .update(&mut visual, |state, cx| state.reconnect(cx));
        visual.run_until_parked();
        let gone =
            visual.update(|window, _| window.try_find_by_label("Discard old tabs").is_none());
        assert!(gone);
    }

    #[gpui_kit::test]
    fn escape_cancels_the_helper_prompt_without_closing_what_is_under_it(cx: &mut TestAppContext) {
        let harness = two_step_helper_prompt(cx);
        let mut visual = open(cx, &harness);
        visual.update(|window, cx| {
            crate::connection_ui::dialog::open_connection_dialog(
                harness.controller.clone(),
                window,
                cx,
            );
        });
        visual.run_until_parked();
        harness.publish_visual(&mut visual, 0, error_frame(HANDSHAKE_FAILURE));
        assert_eq!(
            role_of(&mut visual, "Install and connect"),
            Some(Role::Button)
        );

        visual.simulate_keystrokes("escape");
        visual.run_until_parked();
        assert!(matches!(
            harness.helper(&mut visual),
            HelperUpgradeState::Ready { .. }
        ));
        let helper_gone =
            visual.update(|window, _| window.try_find_by_label("Install and connect").is_none());
        assert!(helper_gone);
        assert_eq!(role_of(&mut visual, "Connect"), Some(Role::Button));
    }

    #[gpui_kit::test]
    fn enter_on_the_focused_install_button_installs(cx: &mut TestAppContext) {
        let harness = two_step_helper_prompt(cx);
        let mut visual = open(cx, &harness);
        harness.publish_visual(&mut visual, 0, error_frame(HANDSHAKE_FAILURE));
        for _ in 0..2 {
            visual.update(|window, cx| {
                window.focus_next(cx);
                window.draw(cx).clear(cx);
            });
        }
        let enter = Keystroke::parse("enter").unwrap();
        visual.simulate_event(KeyDownEvent {
            keystroke: enter.clone(),
            is_held: false,
            prefer_character_input: false,
        });
        visual.simulate_event(KeyUpEvent { keystroke: enter });
        visual.run_until_parked();
        assert_eq!(harness.fake.state().installs.len(), 1);
    }

    #[gpui_kit::test]
    fn enter_with_nothing_focused_keeps_the_connection_form_open(cx: &mut TestAppContext) {
        let harness = two_step_helper_prompt(cx);
        let mut visual = open(cx, &harness);
        visual.update(|window, cx| {
            crate::connection_ui::dialog::open_connection_dialog(
                harness.controller.clone(),
                window,
                cx,
            );
        });
        visual.run_until_parked();

        visual.simulate_keystrokes("enter");
        visual.run_until_parked();
        assert_eq!(role_of(&mut visual, "Connect"), Some(Role::Button));
    }

    fn two_step_helper_prompt(cx: &mut TestAppContext) -> Harness {
        ssh_host(cx, |fake| queue_answer(fake, Ok(probe_of(false, false))))
    }

    impl Harness {
        fn publish_visual(&self, visual: &mut VisualTestContext, index: usize, frame: Vec<u8>) {
            self.fake.publish(index, frame);
            visual.run_until_parked();
            visual.executor().advance_clock(POLL_IDLE);
            visual.run_until_parked();
        }
    }

    #[gpui_kit::test]
    fn host_delete_is_offered_only_while_settings_shows_a_host_to_delete(cx: &mut TestAppContext) {
        let harness = two_shown_hosts(cx);
        let mut visual = open(cx, &harness);
        let offered = |visual: &mut VisualTestContext| {
            visual.update(|_, cx| {
                let commands = cx.global::<crate::commands::Commands>();
                let command = crate::commands::registry::command_by_id("host.delete").unwrap();
                commands.is_available(command)
            })
        };
        assert!(!offered(&mut visual), "no Settings, no picked host");
        visual.update(|window, cx| {
            crate::connection_ui::dialog::open_connection_dialog(
                harness.controller.clone(),
                window,
                cx,
            );
        });
        visual.run_until_parked();
        assert!(offered(&mut visual));

        // The command is the Delete button: it asks before it deletes.
        visual.update(|window, cx| crate::commands::run("host.delete", window, cx));
        visual.run_until_parked();
        assert_eq!(role_of(&mut visual, "Delete host"), Some(Role::Button));
        visual.simulate_keystrokes("escape");
        visual.run_until_parked();
        visual.simulate_keystrokes("escape");
        visual.run_until_parked();
        assert!(!offered(&mut visual), "Settings closed");
    }

    #[gpui_kit::test]
    fn an_open_settings_form_sits_out_a_host_switch_until_it_closes(cx: &mut TestAppContext) {
        let harness = two_shown_hosts(cx);
        let mut visual = open(cx, &harness);
        // Settings is open on the local host, a host being typed in.
        visual.update(|window, cx| {
            crate::connection_ui::dialog::open_connection_dialog(
                harness.controller.clone(),
                window,
                cx,
            );
        });
        visual.run_until_parked();
        harness.update(cx, |state, cx| {
            state.set_ssh_target("typed-host".into(), cx);
            state.activate_host("remote-a", cx);
        });
        harness.read(cx, |state| {
            assert_eq!(state.active_profile_id(), "remote-a");
            assert_eq!(state.selected_profile_id(), "local");
            assert_eq!(state.ssh_target(), "typed-host");
        });
        // Closing Settings catches the form up with the host on screen.
        visual.simulate_keystrokes("escape");
        visual.run_until_parked();
        harness.read(cx, |state| {
            assert_eq!(state.selected_profile_id(), "remote-a");
            assert_eq!(state.ssh_target(), "qa-host");
        });
    }

    /// Port of `App.multiHost.test.tsx`'s "saves a host's letter and
    /// visibility from Settings, without Connect".
    #[gpui_kit::test]
    fn settings_saves_a_letter_without_connect_and_keeps_the_active_host_shown(
        cx: &mut TestAppContext,
    ) {
        let harness = two_shown_hosts(cx);
        let mut visual = open(cx, &harness);
        visual.update(|window, cx| {
            crate::connection_ui::dialog::open_connection_dialog(
                harness.controller.clone(),
                window,
                cx,
            );
        });
        visual.run_until_parked();
        let placeholder = visual.update(|window, _| {
            window
                .find_by_label("Host letter")
                .placeholder()
                .map(str::to_owned)
        });
        assert_eq!(placeholder.as_deref(), Some("L"));
        visual.update(|window, cx| {
            window.click("connection-host-letter", cx);
            window.input("z", cx);
        });
        visual.run_until_parked();
        let local = harness.read(&mut visual, |state| {
            state
                .profiles()
                .iter()
                .find(|profile| profile.id == "local")
                .cloned()
                .unwrap()
        });
        assert_eq!(local.letter.as_deref(), Some("z"));
        assert_eq!(
            crate::connection_ui::host_profiles::host_letter(&local),
            "Z"
        );
        let path = harness._dir.path().join("profiles.json");
        let saved = list_host_profiles(&ProfileStore::load(path).unwrap()).unwrap();
        let on_disk = saved["profiles"]
            .as_array()
            .unwrap()
            .iter()
            .find(|profile| profile["id"] == "local")
            .cloned()
            .unwrap();
        assert_eq!(on_disk["letter"], "z");
        // The host on screen cannot be hidden from here either.
        let shown: Vec<_> = visual.update(|window, _| {
            window
                .find_all_by_label("Show in sidebar")
                .into_iter()
                .filter(|element| element.checked().is_some())
                .collect()
        });
        assert_eq!(shown.len(), 1, "{shown:?}");
        assert_eq!(
            (shown[0].checked(), shown[0].disabled()),
            (Some(true), Some(true))
        );
        // Saved without Connect: no bridge moved and the host on screen stayed.
        assert_eq!(harness.fake.bridge_count(), 2);
        assert!(harness.fake.state().stopped.is_empty());
        let active = harness.read(&mut visual, |state| state.active_profile_id());
        assert_eq!(active, "local");
    }

    #[gpui_kit::test]
    fn the_connection_dialog_names_its_controls_and_connects(cx: &mut TestAppContext) {
        let harness = two_shown_hosts(cx);
        let mut visual = open(cx, &harness);
        visual.update(|window, cx| {
            crate::connection_ui::dialog::open_connection_dialog(
                harness.controller.clone(),
                window,
                cx,
            );
        });
        visual.run_until_parked();
        for label in [
            "Connect",
            "Host",
            "+ Add host",
            "Local",
            "SSH",
            "Show in sidebar",
        ] {
            let found = visual.update(|window, _| window.find_all_by_label(label).len());
            assert!(found >= 1, "{label}");
        }
        assert_eq!(role_of(&mut visual, "Connect"), Some(Role::Button));
    }
}

#[gpui_kit::test]
fn only_the_host_on_screen_may_write_the_clipboard(cx: &mut TestAppContext) {
    let harness = two_shown_hosts(cx);
    let copied = |text: &[u8]| frame(17, "terminal-clipboard", 3, text);
    // The SSH peer is shown, not on screen.
    harness.publish(cx, 1, vec![copied(b"from the peer")]);
    assert_eq!(cx.read_from_clipboard(), None);
    harness.publish(cx, 0, vec![copied(b"from the screen")]);
    assert_eq!(
        cx.read_from_clipboard().and_then(|item| item.text()),
        Some("from the screen".into())
    );
}

#[test]
fn settles_on_no_file_service_event() {
    assert!(!settles(&BridgeEvent::FileService {
        payload: "{}".into()
    }));
    assert!(!settles(&BridgeEvent::Ignored));
    assert!(settles(&BridgeEvent::GitService {
        payload: "{}".into()
    }));
}
