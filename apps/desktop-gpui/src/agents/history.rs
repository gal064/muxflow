//! Session history and snooze (docs/design/chat-plan.md §5 E): each shown
//! host's record of the sessions it has seen, listed when the host connects
//! and again whenever it says its history changed, and the three actions on a
//! session: resume, snooze and wake. The overlay and the Agents list's woke
//! rows draw it (`history_view`).

use std::{collections::HashMap, rc::Rc, sync::Arc};

use desktop_core::connection::history::{
    HistoryAction, HistoryEntry, HistoryEvent, HistoryRequest, HistoryResponse,
};
use futures::future::BoxFuture;
use gpui_kit::{App, AppContext, Context, Entity, Global, Task};

use crate::connection_ui::{
    backend::TerminalBackend,
    connection_reducer::ConnectionPhase,
    controller::{ConnectionController, ConnectionEvent},
};

use super::{
    Agents, BRANCH_PANE_WAIT_MS, PendingOpen,
    model::{AgentRecord, AttentionKind},
    notify::{AgentNativeNotification, NotificationRoute},
    open_when_present, report,
    wire::RequestScope,
};

/// How a host is asked; the connection backend, or a scripted host in tests.
pub type Transport =
    Rc<dyn Fn(String, HistoryRequest) -> BoxFuture<'static, Result<HistoryResponse, String>>>;

/// One host's history as last listed.
#[derive(Clone, Debug, Default)]
pub struct HostHistory {
    /// The connection it was listed over; a new one lists again.
    pub client_id: String,
    pub entries: Vec<HistoryEntry>,
    pub loaded: bool,
    /// Why the latest listing failed, until one succeeds.
    pub failed: Option<String>,
    /// The latest listing asked for: an older answer is dropped.
    asked: u64,
}

pub struct SessionHistory {
    transport: Transport,
    hosts: HashMap<String, HostHistory>,
}

/// The running history, once [`start`] has run.
pub struct History(pub Entity<SessionHistory>);

impl Global for History {}

impl SessionHistory {
    pub fn new(transport: Transport) -> Self {
        Self {
            transport,
            hosts: HashMap::new(),
        }
    }

    pub fn host(&self, profile_id: &str) -> Option<&HostHistory> {
        self.hosts.get(profile_id)
    }

    /// Follows the connected hosts, by profile and client: a new connection
    /// lists its host, and a host no longer connected is forgotten.
    pub fn sync(&mut self, connected: Vec<(String, String)>, cx: &mut Context<Self>) {
        let before = self.hosts.len();
        self.hosts
            .retain(|profile, _| connected.iter().any(|(connected, _)| connected == profile));
        let mut changed = before != self.hosts.len();
        for (profile, client_id) in connected {
            if self
                .hosts
                .get(&profile)
                .is_some_and(|host| host.client_id == client_id)
            {
                continue;
            }
            self.hosts.insert(
                profile.clone(),
                HostHistory {
                    client_id,
                    ..HostHistory::default()
                },
            );
            changed = true;
            self.refresh(&profile, cx);
        }
        if changed {
            cx.notify();
        }
    }

    /// Lists the host's history again.
    pub fn refresh(&mut self, profile_id: &str, cx: &mut Context<Self>) {
        let Some(host) = self.hosts.get_mut(profile_id) else {
            return;
        };
        host.asked += 1;
        let (asked, client_id) = (host.asked, host.client_id.clone());
        let answer = (self.transport)(
            client_id.clone(),
            HistoryRequest {
                action: HistoryAction::List.into(),
                ..HistoryRequest::default()
            },
        );
        let profile_id = profile_id.to_owned();
        cx.spawn(async move |this, cx| {
            let answer = answer.await;
            this.update(cx, |this, cx| {
                let Some(host) = this.hosts.get_mut(&profile_id) else {
                    return;
                };
                if host.client_id != client_id || host.asked != asked {
                    return;
                }
                match answer {
                    Ok(listed) => {
                        host.entries = listed.entries;
                        host.loaded = true;
                        host.failed = None;
                    }
                    Err(error) => host.failed = Some(plain_refusal(&error)),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// The woke sessions of these hosts, by host.
    pub fn woke<'a>(&'a self, hosts: &'a [String]) -> Vec<(&'a str, &'a HistoryEntry)> {
        hosts
            .iter()
            .filter_map(|profile| Some((profile.as_str(), self.hosts.get(profile)?)))
            .flat_map(|(profile, host)| {
                host.entries
                    .iter()
                    .filter(|entry| entry.woke && !entry.running)
                    .map(move |entry| (profile, entry))
            })
            .collect()
    }

    /// One action on a host, answered here.
    pub fn send(
        &self,
        client_id: String,
        request: HistoryRequest,
        cx: &mut Context<Self>,
    ) -> Task<Result<HistoryResponse, String>> {
        let answer = (self.transport)(client_id, request);
        cx.spawn(async move |_, _| answer.await.map_err(|error| plain_refusal(&error)))
    }
}

/// A refusal's display message, without the `history_…:` code before it.
pub fn plain_refusal(error: &str) -> String {
    match error.split_once(": ") {
        Some((code, message)) if code.starts_with("history_") => message.to_owned(),
        _ => error.to_owned(),
    }
}

/// What a session is called: its title, else its provider's name.
pub fn entry_title(entry: &HistoryEntry) -> String {
    if entry.title.trim().is_empty() {
        super::labels::adapter_fallback(&entry.adapter_id).to_owned()
    } else {
        entry.title.clone()
    }
}

/// Starts following the controller's hosts. Called from [`super::start`].
pub fn start(
    controller: &Entity<ConnectionController>,
    backend: Arc<dyn TerminalBackend>,
    cx: &mut App,
) {
    let transport: Transport =
        Rc::new(move |client_id, request| backend.history_request(client_id, request));
    let history = cx.new(|_| SessionHistory::new(transport));
    cx.set_global(History(history.clone()));
    cx.subscribe(controller, {
        let history = history.clone();
        move |_, event, cx| {
            if let ConnectionEvent::History { profile_id, event } = event {
                history.update(cx, |history, cx| history.refresh(profile_id, cx));
                announce_woke(profile_id, event, cx);
            }
        }
    })
    .detach();
    cx.observe(controller, {
        let history = history.clone();
        move |controller, cx| follow(&controller, &history, cx)
    })
    .detach();
    follow(controller, &history, cx);
}

fn follow(
    controller: &Entity<ConnectionController>,
    history: &Entity<SessionHistory>,
    cx: &mut App,
) {
    let connected = controller
        .read(cx)
        .links()
        .filter(|link| link.host_state.phase == ConnectionPhase::Connected)
        .filter_map(|link| Some((link.profile_id.clone(), link.client_id.clone()?)))
        .collect();
    history.update(cx, |history, cx| history.sync(connected, cx));
}

/// A notification for each session whose snooze just ended. Clicking it is
/// not routed anywhere: the placeholder in Agents is what resumes it.
fn announce_woke(profile_id: &str, event: &HistoryEvent, cx: &mut App) {
    let Some(effects) = cx
        .try_global::<Agents>()
        .map(|agents| agents.runtime.read(cx).effects())
    else {
        return;
    };
    for entry in &event.woke {
        let notification = AgentNativeNotification {
            event: AttentionKind::Completed,
            title: format!("{} woke", entry_title(entry)),
            body: if entry.workspace_name.is_empty() {
                "Its snooze is over. Click it in Agents to resume.".to_owned()
            } else {
                format!(
                    "Its snooze is over. Click it in Agents to resume it in {}.",
                    entry.workspace_name
                )
            },
            request_action: false,
            present_in_foreground: true,
            route: NotificationRoute {
                host_profile_id: profile_id.to_owned(),
                server_identity: String::new(),
                session_id: String::new(),
                session_name: entry.workspace_name.clone(),
                window_id: String::new(),
                window_name: String::new(),
                pane_id: String::new(),
                agent_id: String::new(),
                attention_generation: 0,
            },
        };
        let shown = effects.notify(&notification);
        cx.background_executor()
            .spawn(async move {
                if let Err(error) = shown.await {
                    eprintln!("muxflow: could not show the woke notification: {error}");
                }
            })
            .detach();
    }
}

fn scoped(profile_id: &str, cx: &App) -> Result<RequestScope, String> {
    cx.try_global::<Agents>()
        .and_then(|agents| agents.runtime.read(cx).scope(profile_id).cloned())
        .ok_or_else(|| "History needs its host connected and writable.".to_owned())
}

fn request(
    action: HistoryAction,
    adapter_id: &str,
    native_session_id: &str,
    scope: &RequestScope,
) -> HistoryRequest {
    HistoryRequest {
        action: action.into(),
        adapter_id: adapter_id.to_owned(),
        native_session_id: native_session_id.to_owned(),
        expected_server_identity: scope.server_identity.clone(),
        expected_topology_generation: scope.topology_generation,
        ..HistoryRequest::default()
    }
}

/// Sends `request` for the host and lists it again once answered; `done`
/// gets the answer.
fn act(
    profile_id: &str,
    scope: RequestScope,
    request: HistoryRequest,
    done: impl FnOnce(Result<HistoryResponse, String>, &mut App) + 'static,
    cx: &mut App,
) {
    let Some(history) = cx.try_global::<History>().map(|history| history.0.clone()) else {
        return report(cx, "History is not running.".into());
    };
    let task = history.update(cx, |history, cx| history.send(scope.client_id, request, cx));
    let profile_id = profile_id.to_owned();
    cx.spawn(async move |cx| {
        let answer = task.await;
        cx.update(|cx| {
            history.update(cx, |history, cx| history.refresh(&profile_id, cx));
            done(answer, cx);
        });
    })
    .detach();
}

/// Opens the session in a new tab in its workspace, and goes to it.
pub fn resume(profile_id: &str, entry: &HistoryEntry, cx: &mut App) {
    let scope = match scoped(profile_id, cx) {
        Ok(scope) => scope,
        Err(error) => return report(cx, error),
    };
    let title = entry_title(entry);
    report(cx, format!("Resuming {title}…"));
    let now = cx
        .try_global::<Agents>()
        .map_or(0, |agents| agents.runtime.read(cx).now_ms());
    let profile = profile_id.to_owned();
    act(
        profile_id,
        scope.clone(),
        request(
            HistoryAction::Resume,
            &entry.adapter_id,
            &entry.native_session_id,
            &scope,
        ),
        move |answer, cx| match answer {
            Err(error) => report(cx, error),
            Ok(placed) if !placed.pane_id.is_empty() => open_when_present(
                PendingOpen {
                    profile_id: profile,
                    pane_id: placed.pane_id,
                    source: format!("Resumed {title}"),
                    expected_by: now + BRANCH_PANE_WAIT_MS,
                },
                cx,
            ),
            Ok(_) => report(cx, "The host did not say where the session opened.".into()),
        },
        cx,
    );
}

/// Closes the agent's session until `until` (Unix milliseconds).
pub fn snooze(agent: &AgentRecord, until: i64, label: String, cx: &mut App) {
    let scope = match scoped(&agent.host_profile_id, cx) {
        Ok(scope) if scope.server_identity == agent.server_identity => scope,
        Ok(_) => {
            return report(
                cx,
                "This agent's tmux server is no longer the host's.".into(),
            );
        }
        Err(error) => return report(cx, error),
    };
    let name = agent.display_name.clone();
    act(
        &agent.host_profile_id,
        scope.clone(),
        HistoryRequest {
            snooze_until_unix_millis: until,
            ..request(
                HistoryAction::Snooze,
                &agent.adapter_id,
                &agent.native_session_id,
                &scope,
            )
        },
        move |answer, cx| match answer {
            Ok(_) => report(cx, format!("Snoozed {name}: {}.", label.to_lowercase())),
            Err(error) => report(cx, error),
        },
        cx,
    );
}

/// Ends a snooze now; the session becomes a woke placeholder.
pub fn wake(profile_id: &str, entry: &HistoryEntry, cx: &mut App) {
    let scope = match scoped(profile_id, cx) {
        Ok(scope) => scope,
        Err(error) => return report(cx, error),
    };
    act(
        profile_id,
        scope.clone(),
        request(
            HistoryAction::Wake,
            &entry.adapter_id,
            &entry.native_session_id,
            &scope,
        ),
        |answer, cx| {
            if let Err(error) = answer {
                report(cx, error);
            }
        },
        cx,
    );
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use futures::{
        FutureExt,
        channel::oneshot::{self, Sender},
    };
    use gpui_kit::TestAppContext;

    use super::*;

    type Pending = Rc<RefCell<Vec<(String, HistoryRequest, Sender<HistoryResponse>)>>>;

    /// A host whose answers the test hands out, in any order.
    fn host() -> (Pending, Transport) {
        let pending: Pending = Rc::default();
        let transport: Transport = Rc::new({
            let pending = pending.clone();
            move |client, request| {
                let (sender, receiver) = oneshot::channel();
                pending.borrow_mut().push((client, request, sender));
                receiver
                    .map(|answer| answer.map_err(|_| "dropped".to_owned()))
                    .boxed()
            }
        });
        (pending, transport)
    }

    fn listed(titles: &[&str], woke: bool) -> HistoryResponse {
        HistoryResponse {
            entries: titles
                .iter()
                .map(|title| HistoryEntry {
                    adapter_id: "codex".into(),
                    native_session_id: (*title).into(),
                    title: (*title).into(),
                    woke,
                    ..HistoryEntry::default()
                })
                .collect(),
            ..HistoryResponse::default()
        }
    }

    fn answer(pending: &Pending, index: usize, response: HistoryResponse) {
        let (_, _, sender) = pending.borrow_mut().remove(index);
        sender.send(response).unwrap();
    }

    #[gpui_kit::test]
    fn a_new_connection_lists_its_host_and_a_gone_one_is_forgotten(cx: &mut TestAppContext) {
        let (pending, transport) = host();
        let history = cx.new(|_| SessionHistory::new(transport));
        history.update(cx, |history, cx| {
            history.sync(vec![("local".into(), "c1".into())], cx)
        });
        assert_eq!(pending.borrow().len(), 1);
        assert_eq!(pending.borrow()[0].0, "c1");
        assert_eq!(pending.borrow()[0].1.action, i32::from(HistoryAction::List));
        answer(&pending, 0, listed(&["Fix tests"], true));
        cx.run_until_parked();
        history.read_with(cx, |history, _| {
            let shown = ["local".to_owned()];
            let woke = history.woke(&shown);
            assert_eq!(woke.len(), 1);
            assert_eq!(woke[0].1.title, "Fix tests");
        });

        // The same connection again asks nothing; another one lists again.
        history.update(cx, |history, cx| {
            history.sync(vec![("local".into(), "c1".into())], cx)
        });
        assert!(pending.borrow().is_empty());
        history.update(cx, |history, cx| history.sync(Vec::new(), cx));
        history.read_with(cx, |history, _| assert!(history.host("local").is_none()));
    }

    #[gpui_kit::test]
    fn an_older_listing_never_replaces_a_newer_one(cx: &mut TestAppContext) {
        let (pending, transport) = host();
        let history = cx.new(|_| SessionHistory::new(transport));
        history.update(cx, |history, cx| {
            history.sync(vec![("local".into(), "c1".into())], cx);
            history.refresh("local", cx);
        });
        answer(&pending, 1, listed(&["Newer"], false));
        cx.run_until_parked();
        answer(&pending, 0, listed(&["Older"], false));
        cx.run_until_parked();
        history.read_with(cx, |history, _| {
            let host = history.host("local").unwrap();
            assert!(host.loaded);
            assert_eq!(host.entries[0].title, "Newer");
        });
    }

    #[test]
    fn a_refusal_reads_without_its_code() {
        assert_eq!(
            plain_refusal("history_agent_busy: An agent that is working cannot be snoozed."),
            "An agent that is working cannot be snoozed."
        );
        assert_eq!(plain_refusal("connection lost"), "connection lost");
    }
}
