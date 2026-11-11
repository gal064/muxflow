//! The runtime against a scripted host: requests, notifications, attention
//! rings, acknowledgement and the setup flow, with every effect recorded
//! rather than shown or played. Port of `useAgentRuntime.test.tsx` and
//! `useAgentHostSetup.test.tsx`.

use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    rc::Rc,
};

use desktop_core::{
    app_state::{AgentSoundPreferences, HostSetupDecision},
    connection::agent::AgentCommand,
};
use futures::future::{BoxFuture, FutureExt};
use gpui_kit::{AppContext, Entity, TestAppContext};
use serde_json::{Value, json};

use crate::terminal::integration::agents::PaneAttention;

use super::{
    model::{AttentionKind, EventScope},
    notify::{AgentFocus, AgentNativeNotification},
    runtime::{AgentEffects, AgentRuntime, AgentStatus, AgentTransport, RuntimeInputs},
    wire::{HookAction, RequestScope},
};

type Answer = Box<dyn FnMut(&AgentCommand) -> Result<Value, String>>;

/// A host that answers from a script and remembers every request.
struct Host {
    requests: RefCell<Vec<AgentCommand>>,
    answer: RefCell<Answer>,
}

impl Host {
    fn operations(&self) -> Vec<String> {
        self.requests
            .borrow()
            .iter()
            .map(|command| command.operation.clone())
            .collect()
    }
}

struct Transport(Rc<Host>);

impl AgentTransport for Transport {
    fn agent_request(
        &self,
        _: String,
        command: AgentCommand,
    ) -> BoxFuture<'static, Result<Value, String>> {
        let answer = (self.0.answer.borrow_mut())(&command);
        self.0.requests.borrow_mut().push(command);
        futures::future::ready(answer).boxed()
    }
}

#[derive(Default)]
pub(crate) struct Effects {
    shown: RefCell<Vec<AgentNativeNotification>>,
    played: RefCell<Vec<AttentionKind>>,
    decisions: RefCell<Vec<(String, HostSetupDecision)>>,
    now: Cell<u64>,
}

pub(crate) struct Recorded(pub(crate) Rc<Effects>);

impl AgentEffects for Recorded {
    fn notify(
        &self,
        notification: &AgentNativeNotification,
    ) -> BoxFuture<'static, Result<bool, String>> {
        self.0.shown.borrow_mut().push(notification.clone());
        futures::future::ready(Ok(true)).boxed()
    }

    fn play(&self, event: AttentionKind, _: AgentSoundPreferences) {
        self.0.played.borrow_mut().push(event);
    }

    fn now_ms(&self) -> u64 {
        self.0.now.get()
    }

    fn record_decision(&self, host_profile_id: &str, decision: HostSetupDecision) {
        self.0
            .decisions
            .borrow_mut()
            .push((host_profile_id.to_owned(), decision));
    }
}

pub(crate) fn wire_agent(lifecycle: &str, attention: u64, kind: Option<&str>, seen: u64) -> Value {
    json!({
        "agentId": "agent-1", "adapterId": "claude-code", "nativeSessionId": "native",
        "displayName": "Claude",
        "route": {
            "hostProfileId": "local", "serverIdentity": "server-a", "sessionId": "$1",
            "sessionNameFallback": "work", "windowId": "@1", "windowNameFallback": "agent",
            "paneId": "%1", "paneTitleFallback": "", "agentId": "agent-1",
            "attentionGeneration": attention.to_string()
        },
        "lifecycle": lifecycle, "stateGeneration": "5", "attentionGeneration": attention.to_string(),
        "attentionKind": kind, "seenGeneration": seen.to_string(),
        "updatedAtUnixMillis": "100", "lifecycleChangedAtUnixMillis": "90",
        "attentionSeenAtUnixMillis": "0", "detectedManually": false, "present": true
    })
}

pub(crate) fn wire_adapter(wiring: &str) -> Value {
    json!({
        "id": "claude-code", "displayName": "Claude Code", "supportsLaunch": true,
        "supportsResume": true, "supportsHooks": true, "supportsProcessDetection": true,
        "hookConfigPath": "~/.claude/settings.json", "hookEvents": ["Stop"],
        "hookWiring": wiring, "hookSetupRecommended": wiring != "wired"
    })
}

/// The snapshot a host answers `command` with, on the connection it came over.
pub(crate) fn snapshot_answer(
    command: &AgentCommand,
    agents: Vec<Value>,
    adapters: Vec<Value>,
) -> Value {
    json!({ "snapshot": {
        "generation": "6", "acceptedGeneration": "6", "agents": agents,
        "authoritative": true, "notificationWatermark": "4", "connectionEpoch": command.connection_epoch,
        "adapters": adapters
    }})
}

fn hook_plan(already_current: bool) -> Value {
    json!({ "hookPlan": {
        "adapterId": "claude-code", "action": "review", "configPath": "~/.claude/settings.json",
        "backupPath": "", "managedVersion": "muxflow/v1", "summary": "one managed hook",
        "confirmationToken": "confirm", "alreadyCurrent": already_current,
        "ownershipMarker": "muxflow", "proposedCommand": "host hook ingest", "proposedEvents": ["Stop"],
        "trustGuidance": "", "beforeHash": "a", "afterHash": "b", "createsConfig": false,
        "removesConfig": false, "diffPreview": "", "previewTruncated": false
    }})
}

fn scope(epoch: u64, topology: u64) -> RequestScope {
    RequestScope {
        client_id: "client".into(),
        host_profile_id: "local".into(),
        server_identity: "server-a".into(),
        topology_generation: topology,
        connection_epoch: epoch,
    }
}

fn inputs(scopes: Vec<RequestScope>) -> RuntimeInputs {
    RuntimeInputs {
        scopes,
        shown: vec!["local".into()],
        focus: AgentFocus {
            host_profile_id: "local".into(),
            server_identity: "server-a".into(),
            session_id: "$1".into(),
            window_id: "@9".into(),
            pane_id: "%9".into(),
            app_focused: false,
            terminal_visible: true,
        },
        windows: HashMap::new(),
    }
}

struct Harness {
    host: Rc<Host>,
    effects: Rc<Effects>,
    runtime: Entity<AgentRuntime>,
    statuses: Rc<RefCell<Vec<String>>>,
}

impl Harness {
    fn new(
        cx: &mut TestAppContext,
        answer: impl FnMut(&AgentCommand) -> Result<Value, String> + 'static,
    ) -> Self {
        let host = Rc::new(Host {
            requests: RefCell::default(),
            answer: RefCell::new(Box::new(answer)),
        });
        let effects = Rc::new(Effects::default());
        effects.now.set(1_000);
        let runtime = cx.new(|_| {
            let mut runtime = AgentRuntime::new(
                Rc::new(Transport(host.clone())),
                Rc::new(Recorded(effects.clone())),
            );
            runtime.setup.persistable = true;
            runtime.setup.allowed = true;
            runtime.setup.host_label = "This Mac".into();
            runtime
        });
        let statuses = Rc::new(RefCell::new(Vec::new()));
        cx.update(|cx| {
            let statuses = statuses.clone();
            cx.subscribe(&runtime, move |_, AgentStatus(message), _| {
                statuses.borrow_mut().push(message.clone())
            })
            .detach();
        });
        Self {
            host,
            effects,
            runtime,
            statuses,
        }
    }

    fn sync(&self, cx: &mut TestAppContext, inputs: RuntimeInputs) {
        self.runtime
            .update(cx, |runtime, cx| runtime.sync(inputs, cx));
        cx.run_until_parked();
    }

    fn push(&self, cx: &mut TestAppContext, epoch: u64, payload: Value) {
        let scope = EventScope {
            host_profile_id: "local".into(),
            server_identity: "server-a".into(),
            connection_epoch: epoch,
        };
        self.runtime.update(cx, |runtime, cx| {
            runtime.pushed(scope, "agent", &payload.to_string(), cx)
        });
        cx.run_until_parked();
    }

    fn lifecycles(&self, cx: &mut TestAppContext) -> Vec<(String, u64)> {
        self.runtime.read_with(cx, |runtime, _| {
            runtime
                .agents("local")
                .into_iter()
                .map(|agent| (agent.id.clone(), agent.attention_generation))
                .collect()
        })
    }

    fn rings(&self, cx: &mut TestAppContext) -> Vec<String> {
        self.runtime.read_with(cx, |runtime, _| {
            runtime
                .attention_rings()
                .get("local")
                .cloned()
                .unwrap_or_default()
        })
    }
}

fn wants(cx: &mut TestAppContext, pane: &str) -> bool {
    cx.update(|cx| {
        cx.try_global::<PaneAttention>()
            .is_some_and(|attention| attention.wants("local", pane))
    })
}

/// A host whose snapshot is `agents`, and that accepts every seen mark on a
/// finished turn.
fn snapshot_host(agents: Vec<Value>) -> impl FnMut(&AgentCommand) -> Result<Value, String> {
    move |command| match command.operation.as_str() {
        "snapshot" => Ok(snapshot_answer(
            command,
            agents.clone(),
            vec![wire_adapter("wired")],
        )),
        "markSeen" => {
            let generation: u64 = command.attention_generation.parse().unwrap();
            Ok(json!({
                "agent": wire_agent("idle", generation, Some("completed"), generation),
                "acceptedGeneration": "9",
            }))
        }
        other => Err(format!("unexpected {other}")),
    }
}

#[gpui_kit::test]
fn asks_each_scope_once_and_again_on_every_topology_change(cx: &mut TestAppContext) {
    let harness = Harness::new(cx, snapshot_host(vec![wire_agent("working", 0, None, 0)]));
    harness.sync(cx, inputs(vec![scope(4, 1)]));
    harness.sync(cx, inputs(vec![scope(4, 1)]));
    assert_eq!(harness.host.operations(), ["snapshot"]);
    let first = harness.host.requests.borrow()[0].clone();
    assert_eq!(
        (
            first.expected_server_identity.as_str(),
            first.connection_epoch.as_str(),
            first.expected_topology_generation.as_str()
        ),
        ("server-a", "4", "1")
    );
    assert_eq!(harness.lifecycles(cx), [("agent-1".to_owned(), 0)]);

    harness.sync(cx, inputs(vec![scope(4, 2)]));
    assert_eq!(harness.host.operations(), ["snapshot", "snapshot"]);
}

#[gpui_kit::test]
fn a_new_blocked_turn_notifies_and_plays_once_and_a_replay_does_neither(cx: &mut TestAppContext) {
    let harness = Harness::new(cx, snapshot_host(vec![wire_agent("working", 0, None, 0)]));
    harness.sync(cx, inputs(vec![scope(4, 1)]));
    // First sight of a server is old news.
    assert!(harness.effects.shown.borrow().is_empty());

    harness.push(cx, 4, json!({ "agent": wire_agent("blocked", 7, Some("blocked"), 0), "generation": "7", "connectionEpoch": "4" }));
    assert_eq!(*harness.effects.played.borrow(), [AttentionKind::Blocked]);
    assert_eq!(harness.effects.shown.borrow().len(), 1);
    assert_eq!(
        harness.effects.shown.borrow()[0].event,
        AttentionKind::Blocked
    );

    // The same generation again, and a replayed event, are not new.
    harness.push(cx, 4, json!({ "agent": wire_agent("blocked", 7, Some("blocked"), 0), "generation": "8", "connectionEpoch": "4" }));
    harness.push(cx, 4, json!({ "agent": wire_agent("blocked", 9, Some("blocked"), 0), "generation": "9", "connectionEpoch": "4", "notify": false }));
    assert_eq!(harness.effects.shown.borrow().len(), 1);
    assert_eq!(harness.effects.played.borrow().len(), 1);
}

#[gpui_kit::test]
fn a_frame_from_another_connection_epoch_is_dropped(cx: &mut TestAppContext) {
    let harness = Harness::new(cx, snapshot_host(vec![wire_agent("working", 0, None, 0)]));
    harness.sync(cx, inputs(vec![scope(4, 1)]));
    harness.push(cx, 3, json!({ "agent": wire_agent("blocked", 7, Some("blocked"), 0), "generation": "7", "connectionEpoch": "3" }));
    assert_eq!(harness.lifecycles(cx), [("agent-1".to_owned(), 0)]);
    assert!(harness.effects.shown.borrow().is_empty());
}

#[gpui_kit::test]
fn rings_a_waiting_pane_and_typing_in_it_acknowledges_the_exact_generation(
    cx: &mut TestAppContext,
) {
    let harness = Harness::new(
        cx,
        snapshot_host(vec![wire_agent("idle", 7, Some("completed"), 0)]),
    );
    harness.sync(cx, inputs(vec![scope(4, 1)]));
    assert_eq!(harness.rings(cx), ["%1"]);
    assert!(wants(cx, "%1"));

    // Activity elsewhere acknowledges nothing.
    harness.runtime.update(cx, |runtime, cx| {
        runtime.acknowledge_pane("local", "%2", cx)
    });
    cx.run_until_parked();
    assert_eq!(harness.host.operations(), ["snapshot"]);

    harness.runtime.update(cx, |runtime, cx| {
        runtime.acknowledge_pane("local", "%1", cx);
        // A second key before the answer is not a second request.
        runtime.acknowledge_pane("local", "%1", cx);
    });
    cx.run_until_parked();
    assert_eq!(harness.host.operations(), ["snapshot", "markSeen"]);
    let seen = harness.host.requests.borrow()[1].clone();
    assert_eq!(
        (seen.agent_id.as_str(), seen.attention_generation.as_str()),
        ("agent-1", "7")
    );
    assert!(harness.rings(cx).is_empty());
    assert!(!wants(cx, "%1"));
}

#[gpui_kit::test]
fn a_refused_seen_mark_keeps_the_ring_and_says_why(cx: &mut TestAppContext) {
    let harness = Harness::new(cx, |command| match command.operation.as_str() {
        "snapshot" => Ok(snapshot_answer(
            command,
            vec![wire_agent("blocked", 7, Some("blocked"), 0)],
            vec![],
        )),
        _ => Err("stale generation".into()),
    });
    harness.sync(cx, inputs(vec![scope(4, 1)]));
    harness.runtime.update(cx, |runtime, cx| {
        runtime.acknowledge_pane("local", "%1", cx)
    });
    cx.run_until_parked();
    assert_eq!(harness.rings(cx), ["%1"]);
    assert_eq!(
        *harness.statuses.borrow(),
        ["Could not mark agent attention seen: stale generation"]
    );
}

#[gpui_kit::test]
fn a_lost_client_disconnects_its_host_and_a_hidden_host_is_removed(cx: &mut TestAppContext) {
    let harness = Harness::new(
        cx,
        snapshot_host(vec![wire_agent("blocked", 7, Some("blocked"), 0)]),
    );
    harness.sync(cx, inputs(vec![scope(4, 1)]));
    assert_eq!(harness.lifecycles(cx).len(), 1);

    harness.sync(cx, inputs(vec![]));
    assert!(harness.lifecycles(cx).is_empty());
    assert!(harness.rings(cx).is_empty());

    harness.sync(cx, inputs(vec![scope(5, 1)]));
    assert_eq!(harness.lifecycles(cx).len(), 1);
    let mut hidden = inputs(vec![]);
    hidden.shown.clear();
    harness.sync(cx, hidden);
    assert!(
        harness
            .runtime
            .read_with(cx, |runtime, _| runtime.store.host("local").is_none())
    );
    assert!(!wants(cx, "%1"));
}

#[gpui_kit::test]
fn a_hook_request_for_another_host_is_refused_without_a_request(cx: &mut TestAppContext) {
    let harness = Harness::new(cx, snapshot_host(vec![]));
    harness.sync(cx, inputs(vec![scope(4, 1)]));
    let refused = harness.runtime.update(cx, |runtime, cx| {
        runtime.review_hooks(
            "claude-code",
            HookAction::Install,
            "local\u{0}client\u{0}3",
            cx,
        )
    });
    let error = cx.foreground_executor().block_test(refused).unwrap_err();
    assert!(error.contains("different host"), "{error}");
    assert_eq!(harness.host.operations(), ["snapshot"]);
}

/// A host whose Claude hooks are `wiring` and that accepts installs.
fn setup_host(wiring: &'static str) -> impl FnMut(&AgentCommand) -> Result<Value, String> {
    let installed = Rc::new(Cell::new(false));
    move |command| match command.operation.as_str() {
        "snapshot" => Ok(snapshot_answer(
            command,
            vec![],
            vec![wire_adapter(if installed.get() { "wired" } else { wiring })],
        )),
        "hookReview" => Ok(hook_plan(installed.get())),
        "hookInstall" => {
            installed.set(true);
            Ok(json!({}))
        }
        "hostNaming" => Ok(json!({ "hostNaming": "applied" })),
        other => Err(format!("unexpected {other}")),
    }
}

fn question_open(harness: &Harness, cx: &mut TestAppContext) -> bool {
    harness
        .runtime
        .read_with(cx, |runtime, _| runtime.setup.question.is_some())
}

#[gpui_kit::test]
fn asks_once_on_a_host_that_cannot_report_and_remembers_not_now(cx: &mut TestAppContext) {
    let harness = Harness::new(cx, setup_host("notWired"));
    harness.sync(cx, inputs(vec![scope(4, 1)]));
    assert!(question_open(&harness, cx));
    let question = harness
        .runtime
        .read_with(cx, |runtime, _| runtime.setup.question.clone().unwrap());
    assert_eq!(question.label, "This Mac");
    assert_eq!(question.targets.len(), 1);

    harness
        .runtime
        .update(cx, |runtime, cx| runtime.decline_setup(cx));
    cx.run_until_parked();
    assert!(!question_open(&harness, cx));
    assert_eq!(
        *harness.effects.decisions.borrow(),
        [("local".to_owned(), HostSetupDecision::Declined)]
    );

    // A new connection to the same host does not ask again.
    harness.sync(cx, inputs(vec![scope(5, 1)]));
    assert!(!question_open(&harness, cx));
    assert_eq!(harness.host.operations(), ["snapshot", "snapshot"]);
}

#[gpui_kit::test]
fn setting_up_installs_records_the_answer_and_names_windows(cx: &mut TestAppContext) {
    let harness = Harness::new(cx, setup_host("notWired"));
    harness.sync(cx, inputs(vec![scope(4, 1)]));
    harness
        .runtime
        .update(cx, |runtime, cx| runtime.accept_setup(cx));
    cx.run_until_parked();
    assert!(!question_open(&harness, cx));
    assert_eq!(
        *harness.effects.decisions.borrow(),
        [("local".to_owned(), HostSetupDecision::Accepted)]
    );
    // The install, then the naming, then a fresh snapshot that now reports.
    assert_eq!(
        harness.host.operations(),
        [
            "snapshot",
            "hookReview",
            "hookInstall",
            "hostNaming",
            "snapshot"
        ]
    );
    let install = harness.host.requests.borrow()[2].clone();
    assert_eq!(
        (install.confirmed, install.confirmation_token.as_str()),
        (true, "confirm")
    );
    assert!(
        harness
            .statuses
            .borrow()
            .contains(&"Agent status hooks installed on This Mac.".to_owned())
    );
    assert!(
        harness
            .runtime
            .read_with(cx, |runtime, _| !runtime.setup_offerable())
    );
}

#[gpui_kit::test]
fn an_answer_cannot_be_recorded_when_app_state_is_frozen(cx: &mut TestAppContext) {
    let harness = Harness::new(cx, setup_host("notWired"));
    harness
        .runtime
        .update(cx, |runtime, _| runtime.setup.persistable = false);
    harness.sync(cx, inputs(vec![scope(4, 1)]));
    harness
        .runtime
        .update(cx, |runtime, cx| runtime.accept_setup(cx));
    cx.run_until_parked();
    let error = harness.runtime.read_with(cx, |runtime, _| {
        runtime
            .setup
            .question
            .as_ref()
            .and_then(|question| question.error.clone())
    });
    assert_eq!(
        error.as_deref(),
        Some(
            "Your saved settings could not be read, so this answer could not be recorded; nothing was changed."
        )
    );
    assert_eq!(harness.host.operations(), ["snapshot"]);
}

#[gpui_kit::test]
fn the_helper_install_answers_for_agent_status_without_asking_twice(cx: &mut TestAppContext) {
    let harness = Harness::new(cx, setup_host("notWired"));
    harness.runtime.update(cx, |runtime, _| {
        runtime.setup.auto_setup = Some("local".into())
    });
    harness.sync(cx, inputs(vec![scope(4, 1)]));
    assert!(!question_open(&harness, cx));
    assert_eq!(
        *harness.effects.decisions.borrow(),
        [("local".to_owned(), HostSetupDecision::Accepted)]
    );
    assert!(
        harness
            .host
            .operations()
            .contains(&"hookInstall".to_owned())
    );
}

#[gpui_kit::test]
fn a_consented_host_with_partial_hooks_is_brought_up_to_date_once(cx: &mut TestAppContext) {
    // Before the snapshot there is nothing to install, so the naming is
    // asserted alone; the install that follows asserts it again, as in Tauri.
    let harness = Harness::new(cx, setup_host("partial"));
    harness.runtime.update(cx, |runtime, _| {
        runtime
            .setup
            .decisions
            .insert("local".into(), HostSetupDecision::Accepted)
    });
    harness.sync(cx, inputs(vec![scope(4, 1)]));
    assert!(!question_open(&harness, cx));
    assert_eq!(
        harness.host.operations(),
        [
            "snapshot",
            "hostNaming",
            "hookReview",
            "hookInstall",
            "hostNaming",
            "snapshot"
        ]
    );
    assert!(
        harness
            .statuses
            .borrow()
            .contains(&"Updated the agent status hooks for Claude Code on this host.".to_owned())
    );
    // The topology moving again is not a second install.
    harness.sync(cx, inputs(vec![scope(4, 2)]));
    assert_eq!(
        harness
            .host
            .operations()
            .iter()
            .filter(|op| *op == "hookInstall")
            .count(),
        1
    );
}

#[gpui_kit::test]
fn a_reconnect_snapshot_catches_up_once_on_what_happened_while_away(cx: &mut TestAppContext) {
    let asked = Rc::new(Cell::new(0));
    let answers = asked.clone();
    let harness = Harness::new(cx, move |command| {
        answers.set(answers.get() + 1);
        let (agent, watermark) = if answers.get() == 1 {
            (wire_agent("working", 0, None, 0), "4")
        } else {
            (wire_agent("blocked", 7, Some("blocked"), 0), "7")
        };
        let mut answer = snapshot_answer(command, vec![agent], vec![wire_adapter("wired")]);
        answer["snapshot"]["notificationWatermark"] = json!(watermark);
        Ok(answer)
    });
    harness.sync(cx, inputs(vec![scope(4, 1)]));
    assert!(harness.effects.shown.borrow().is_empty());

    // Away: the client went, then came back on a new connection.
    harness.sync(cx, inputs(vec![]));
    harness.sync(cx, inputs(vec![scope(5, 1)]));
    assert_eq!(harness.effects.shown.borrow().len(), 1);
    assert_eq!(harness.rings(cx), ["%1"]);

    // The same watermark again is nothing new.
    harness.sync(cx, inputs(vec![scope(5, 2)]));
    assert_eq!(asked.get(), 3);
    assert_eq!(harness.effects.shown.borrow().len(), 1);
}

#[gpui_kit::test]
fn the_shell_shows_the_current_servers_agents_and_nothing_after_it_changes(
    cx: &mut TestAppContext,
) {
    use crate::workspaces::agents::AgentPresence;

    let harness = Harness::new(
        cx,
        snapshot_host(vec![wire_agent("blocked", 7, Some("blocked"), 0)]),
    );
    harness.sync(cx, inputs(vec![scope(4, 1)]));
    let shell = harness
        .runtime
        .read_with(cx, |runtime, _| super::agent_shell_from(runtime));
    let row = shell
        .workspace("local", "$1")
        .expect("the agent's workspace");
    assert_eq!(row.agents.len(), 1);
    assert_eq!(row.agents[0].state, super::model::DisplayState::Blocked);
    assert_eq!((row.unread, row.working, row.overflow), (1, false, 0));
    assert_eq!(row.state, Some(super::model::DisplayState::Blocked));
    let window = shell.window("local", "@1").expect("the agent's window");
    assert_eq!(window.presence, AgentPresence::Present);
    assert_eq!(
        window.attention,
        Some((super::model::DisplayState::Blocked, "claude-code".into()))
    );
    // A window with no agent is not known to have none.
    assert!(shell.window("local", "@2").is_none());

    // The host's tmux server was replaced: its old agents say nothing.
    let mut other = inputs(vec![RequestScope {
        server_identity: "server-b".into(),
        ..scope(5, 1)
    }]);
    other.focus.server_identity = "server-b".into();
    harness.sync(cx, other);
    let shell = harness
        .runtime
        .read_with(cx, |runtime, _| super::agent_shell_from(runtime));
    assert_eq!(shell, crate::workspaces::agents::AgentShell::default());
}

/// Port of the topology authority in `useAgentRuntime.ts`: a window is known
/// to hold no agent only from a snapshot asked for against the topology on
/// screen, and only until tmux moves again.
#[gpui_kit::test]
fn a_window_without_an_agent_is_absent_only_under_the_topology_it_was_asked_against(
    cx: &mut TestAppContext,
) {
    use crate::workspaces::agents::AgentPresence;

    let unmapped = std::rc::Rc::new(Cell::new(false));
    let harness = Harness::new(cx, {
        let unmapped = unmapped.clone();
        move |command| {
            let mut agent = wire_agent("working", 0, None, 0);
            if unmapped.get() {
                agent["route"]["paneId"] = json!("");
                agent["route"]["windowId"] = json!("");
            }
            Ok(snapshot_answer(
                command,
                vec![agent],
                vec![wire_adapter("wired")],
            ))
        }
    });
    let covering = |epoch: u64, topology: u64| RuntimeInputs {
        windows: HashMap::from([("local".into(), vec!["@1".into(), "@2".into()])]),
        ..inputs(vec![scope(epoch, topology)])
    };
    let shell = |cx: &mut TestAppContext| {
        harness
            .runtime
            .read_with(cx, |runtime, _| super::agent_shell_from(runtime))
    };
    let presence = |shell: &crate::workspaces::agents::AgentShell, window: &str| {
        shell.window("local", window).map(|window| window.presence)
    };
    harness.sync(cx, covering(4, 1));
    let now = shell(cx);
    assert_eq!(presence(&now, "@1"), Some(AgentPresence::Present));
    assert_eq!(presence(&now, "@2"), Some(AgentPresence::Absent));
    // A window the snapshot was not asked about proves nothing.
    assert_eq!(presence(&now, "@3"), None);
    assert!(now.current_through("local", 1));

    // tmux moved: until the snapshot asked for now answers, nothing is absent.
    harness
        .runtime
        .update(cx, |runtime, cx| runtime.sync(covering(4, 2), cx));
    let moving = shell(cx);
    assert_eq!(presence(&moving, "@2"), None);
    assert!(!moving.current_through("local", 1));
    cx.run_until_parked();
    let now = shell(cx);
    assert_eq!(presence(&now, "@2"), Some(AgentPresence::Absent));
    assert!(now.current_through("local", 2));

    // Nor does what the last connection saw, with the same windows.
    harness
        .runtime
        .update(cx, |runtime, cx| runtime.sync(covering(5, 2), cx));
    assert_eq!(presence(&shell(cx), "@2"), None);
    cx.run_until_parked();
    assert_eq!(presence(&shell(cx), "@2"), Some(AgentPresence::Absent));

    // An agent with no window could be in any of them.
    unmapped.set(true);
    harness.sync(cx, covering(5, 3));
    assert_eq!(presence(&shell(cx), "@2"), None);
}

#[gpui_kit::test]
fn settings_offers_this_runtimes_agent_setup(cx: &mut TestAppContext) {
    use crate::settings::{AgentSetupStatus, agent_setup};

    let harness = Harness::new(cx, setup_host("notWired"));
    cx.update(|cx| super::publish_agent_setup(&harness.runtime, cx));
    let status =
        |cx: &mut TestAppContext| cx.update(|cx| (agent_setup(cx).expect("published").status)(cx));
    assert_eq!(
        status(cx),
        AgentSetupStatus {
            available: false,
            reports: false,
            connected: false
        }
    );
    harness.sync(cx, inputs(vec![scope(4, 1)]));
    harness
        .runtime
        .update(cx, |runtime, cx| runtime.decline_setup(cx));
    cx.run_until_parked();
    assert!(!question_open(&harness, cx));
    assert_eq!(
        status(cx),
        AgentSetupStatus {
            available: true,
            reports: false,
            connected: true
        }
    );
    // The button in Settings asks again, even after "Not now".
    let set_up = cx.update(|cx| agent_setup(cx).expect("published").set_up);
    let visual = cx.add_empty_window();
    visual.update(|window, cx| set_up(window, cx));
    cx.run_until_parked();
    assert!(question_open(&harness, cx));
}
